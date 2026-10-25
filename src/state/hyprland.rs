use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use anyhow::Context;
use serde::Deserialize;

use super::Window;
use crate::daemon::Event;

#[derive(Deserialize, Default)]
struct WindowJson {
    #[serde(default)]
    class: String,
    #[serde(default)]
    title: String,
}

// .socket.sock answers commands, .socket2.sock streams events one per line.
fn socket_dir() -> anyhow::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").context("not running under Hyprland")?;
    Ok(PathBuf::from(runtime).join("hypr").join(sig))
}

fn ask(dir: &Path, command: &str) -> anyhow::Result<String> {
    let mut socket = UnixStream::connect(dir.join(".socket.sock"))?;
    socket.write_all(command.as_bytes())?;
    let mut reply = String::new();
    socket.read_to_string(&mut reply)?;
    Ok(reply)
}

fn current_window(dir: &Path) -> anyhow::Result<Window> {
    let window: WindowJson = serde_json::from_str(&ask(dir, "j/activewindow")?)?;
    Ok(Window {
        class: window.class,
        title: window.title,
    })
}

fn is_focus_event(line: &str) -> bool {
    line.starts_with("activewindow") || line.starts_with("windowtitle")
}

pub fn spawn_watcher(tx: Sender<Event>) -> anyhow::Result<()> {
    let dir = socket_dir()?;
    let events =
        UnixStream::connect(dir.join(".socket2.sock")).context("connecting to Hyprland")?;
    tracing::info!("focus: Hyprland at {}", dir.display());

    tx.send(Event::Focus(current_window(&dir)?))?;
    std::thread::spawn(move || {
        for line in BufReader::new(events).lines() {
            let line = match line {
                Ok(line) => line,
                Err(e) => {
                    tracing::error!("Hyprland socket closed: {e}");
                    return;
                }
            };
            if !is_focus_event(&line) {
                continue;
            }
            match current_window(&dir) {
                Ok(window) => {
                    if tx.send(Event::Focus(window)).is_err() {
                        return;
                    }
                }
                Err(e) => tracing::warn!("Hyprland window: {e:#}"),
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_which_events_change_the_focused_window() {
        assert!(is_focus_event(
            "activewindow>>firefox,GitHub - Mozilla Firefox"
        ));
        assert!(is_focus_event("windowtitlev2>>55d2c8678d10,New Tab"));
        assert!(!is_focus_event("workspace>>2"));
    }

    #[test]
    fn reads_the_focused_window_even_when_nothing_is_focused() {
        let window: WindowJson =
            serde_json::from_str(r#"{"address":"0x1","class":"kitty","title":"x"}"#).unwrap();
        assert_eq!(
            (window.class.as_str(), window.title.as_str()),
            ("kitty", "x")
        );
        let none: WindowJson = serde_json::from_str("{}").unwrap();
        assert_eq!(none.class, "");
    }
}
