use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use anyhow::Context;
use serde::Deserialize;

use super::{Placed, Window, Workspaces};
use crate::daemon::Event;

// The fields read from Hyprland's JSON replies.
#[derive(Deserialize, Default)]
struct WindowJson {
    #[serde(default)]
    class: String,
    #[serde(default)]
    title: String,
    #[serde(default, rename = "stableId")]
    stable_id: String,
    #[serde(default)]
    workspace: WorkspaceRef,
    #[serde(default)]
    at: [f64; 2],
    #[serde(default)]
    size: [f64; 2],
    #[serde(default)]
    floating: bool,
    #[serde(default)]
    mapped: bool,
    #[serde(default)]
    hidden: bool,
}

#[derive(Deserialize, Default)]
struct WorkspaceRef {
    #[serde(default)]
    id: i32,
}

#[derive(Deserialize)]
struct WorkspaceJson {
    id: i32,
    #[serde(default)]
    monitor: String,
}

#[derive(Deserialize)]
struct MonitorJson {
    name: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale: f64,
}

// Hyprland has two sockets: .socket.sock answers commands and
// .socket2.sock streams events, one per line.
fn socket_dir() -> anyhow::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").context("not running under Hyprland")?;
    Ok(PathBuf::from(runtime).join("hypr").join(sig))
}

// Sends one command and returns the reply.
fn ask(dir: &Path, command: &str) -> anyhow::Result<String> {
    let mut socket = UnixStream::connect(dir.join(".socket.sock"))?;
    socket.write_all(command.as_bytes())?;
    let mut reply = String::new();
    socket.read_to_string(&mut reply)?;
    Ok(reply)
}

fn current_window(dir: &Path) -> anyhow::Result<Window> {
    let window: WindowJson = serde_json::from_str(&ask(dir, "j/activewindow")?)?;
    Ok(Window { class: window.class, title: window.title })
}

fn workspaces(dir: &Path) -> anyhow::Result<Workspaces> {
    let list: Vec<WorkspaceJson> = serde_json::from_str(&ask(dir, "j/workspaces")?)?;
    let active: WorkspaceJson = serde_json::from_str(&ask(dir, "j/activeworkspace")?)?;
    let clients: Vec<WindowJson> = serde_json::from_str(&ask(dir, "j/clients")?)?;
    let monitors: Vec<MonitorJson> = serde_json::from_str(&ask(dir, "j/monitors")?)?;
    Ok(build(list, active, clients, monitors))
}

// Special workspaces have negative ids and are left out.
fn build(list: Vec<WorkspaceJson>, active: WorkspaceJson, clients: Vec<WindowJson>, monitors: Vec<MonitorJson>) -> Workspaces {
    let monitor = monitors.iter().find(|m| m.name == active.monitor).or(monitors.first());
    let (origin, screen) = match monitor {
        Some(m) => ((m.x, m.y), (m.width / m.scale, m.height / m.scale)),
        None => ((0.0, 0.0), (1.0, 1.0)),
    };

    let mut ids: Vec<i32> = list.iter().map(|w| w.id).filter(|id| *id > 0).collect();
    ids.sort();

    // Floating windows go last so they draw on top.
    let mut clients: Vec<WindowJson> = clients.into_iter().filter(|w| w.mapped && !w.hidden).collect();
    clients.sort_by_key(|w| w.floating);
    let mut windows: HashMap<i32, Vec<Placed>> = HashMap::new();
    for w in clients {
        windows.entry(w.workspace.id).or_default().push(Placed {
            window: w.stable_id,
            x: w.at[0] - origin.0,
            y: w.at[1] - origin.1,
            width: w.size[0],
            height: w.size[1],
        });
    }
    Workspaces { ids, active: active.id, monitor: active.monitor, screen, windows, ..Default::default() }
}

fn is_focus_event(line: &str) -> bool {
    line.starts_with("activewindow") || line.starts_with("windowtitle")
}

fn is_workspace_event(line: &str) -> bool {
    ["workspace", "createworkspace", "destroyworkspace", "focusedmon"]
        .iter()
        .any(|name| line.starts_with(name))
}

pub fn spawn_watcher(tx: Sender<Event>) -> anyhow::Result<()> {
    let dir = socket_dir()?;
    let events = UnixStream::connect(dir.join(".socket2.sock")).context("connecting to Hyprland")?;
    tracing::info!("focus: Hyprland at {}", dir.display());

    tx.send(Event::Focus(current_window(&dir)?))?;
    tx.send(Event::Workspaces(workspaces(&dir)?))?;
    std::thread::spawn(move || {
        for line in BufReader::new(events).lines() {
            let line = match line {
                Ok(line) => line,
                Err(e) => {
                    tracing::error!("Hyprland socket closed: {e}");
                    return;
                }
            };
            let focus = is_focus_event(&line);
            if focus {
                match current_window(&dir) {
                    Ok(window) => {
                        if tx.send(Event::Focus(window)).is_err() {
                            return;
                        }
                    }
                    Err(e) => tracing::warn!("Hyprland window: {e:#}"),
                }
            }
            if focus || is_workspace_event(&line) {
                match workspaces(&dir) {
                    Ok(workspaces) => {
                        if tx.send(Event::Workspaces(workspaces)).is_err() {
                            return;
                        }
                    }
                    Err(e) => tracing::warn!("Hyprland workspaces: {e:#}"),
                }
            }
        }
    });
    Ok(())
}

pub fn switch_workspace(id: i32) -> anyhow::Result<()> {
    let dir = socket_dir()?;
    // Hyprland 0.56 and later take Lua; older versions take plain words.
    let reply = ask(&dir, &format!("dispatch hl.dsp.focus({{ workspace = {id} }})"))?;
    if reply.trim() != "ok" {
        ask(&dir, &format!("dispatch workspace {id}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_workspaces_with_their_windows_to_scale() {
        let ws = |id, monitor: &str| WorkspaceJson { id, monitor: monitor.to_string() };
        let list = vec![ws(3, "eDP-1"), ws(1, "eDP-1"), ws(-99, "eDP-1"), ws(2, "eDP-1")];
        let client = |workspace, id: &str, x, floating| WindowJson {
            stable_id: id.to_string(),
            workspace: WorkspaceRef { id: workspace },
            at: [x, 1.0],
            size: [718.0, 898.0],
            floating,
            mapped: true,
            ..Default::default()
        };
        let clients = vec![client(1, "float", 100.0, true), client(1, "a", 1.0, false), client(3, "b", 721.0, false)];
        let monitors = vec![MonitorJson { name: "eDP-1".to_string(), x: 0.0, y: 0.0, width: 2880.0, height: 1800.0, scale: 2.0 }];
        let ws = build(list, ws(3, "eDP-1"), clients, monitors);
        assert_eq!(ws.ids, vec![1, 2, 3]);
        assert_eq!(ws.active, 3);
        assert_eq!(ws.screen, (1440.0, 900.0));
        let on_one: Vec<&str> = ws.windows[&1].iter().map(|p| p.window.as_str()).collect();
        assert_eq!(on_one, ["a", "float"]);
        assert_eq!(ws.windows[&3][0].x, 721.0);
        assert!(ws.windows.get(&2).is_none());
        assert_eq!(ws.all_shots().len(), 2);
    }

    #[test]
    fn knows_which_events_matter() {
        assert!(is_focus_event("activewindow>>firefox,GitHub - Mozilla Firefox"));
        assert!(is_focus_event("windowtitlev2>>55d2c8678d10,New Tab"));
        assert!(is_workspace_event("workspacev2>>2,2"));
        assert!(!is_focus_event("workspace>>2"));
        assert!(!is_workspace_event("activewindow>>kitty,shell"));
    }

    #[test]
    fn reads_the_focused_window_even_when_nothing_is_focused() {
        let window: WindowJson = serde_json::from_str(r#"{"address":"0x1","class":"kitty","title":"x"}"#).unwrap();
        assert_eq!((window.class.as_str(), window.title.as_str()), ("kitty", "x"));
        let none: WindowJson = serde_json::from_str("{}").unwrap();
        assert_eq!(none.class, "");
    }
}
