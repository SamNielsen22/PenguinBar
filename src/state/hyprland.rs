use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use anyhow::Context;

use crate::daemon::Event;

// Hyprland keeps two sockets: .socket.sock answers commands and
// .socket2.sock streams events, one per line, like
//     activewindow>>firefox,GitHub - Mozilla Firefox
fn socket_dir() -> anyhow::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").context("not running under Hyprland")?;
    Ok(PathBuf::from(runtime).join("hypr").join(sig))
}

// Ask which window is focused right now.
fn current_class(dir: &Path) -> anyhow::Result<String> {
    let mut socket = UnixStream::connect(dir.join(".socket.sock"))?;
    socket.write_all(b"activewindow")?;
    let mut reply = String::new();
    socket.read_to_string(&mut reply)?;
    Ok(class_in_reply(&reply).to_string())
}

// The reply is one "name: value" per line; we want the class line.
fn class_in_reply(reply: &str) -> &str {
    reply
        .lines()
        .find_map(|line| line.trim().strip_prefix("class: "))
        .unwrap_or("")
}

// Only focus changes matter. The line is "activewindow>>class,title".
fn class_in_event(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("activewindow>>")?;
    rest.split(',').next()
}

pub fn spawn_watcher(tx: Sender<Event>) -> anyhow::Result<()> {
    let dir = socket_dir()?;
    let events = UnixStream::connect(dir.join(".socket2.sock")).context("connecting to Hyprland")?;
    tracing::info!("focus: Hyprland at {}", dir.display());

    tx.send(Event::Focus(current_class(&dir)?))?;
    std::thread::spawn(move || {
        for line in BufReader::new(events).lines() {
            let line = match line {
                Ok(line) => line,
                Err(e) => {
                    tracing::error!("Hyprland socket closed: {e}");
                    return;
                }
            };
            if let Some(class) = class_in_event(&line) {
                if tx.send(Event::Focus(class.to_string())).is_err() {
                    return;
                }
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_class_from_a_focus_event() {
        assert_eq!(class_in_event("activewindow>>firefox,GitHub - Mozilla Firefox"), Some("firefox"));
        assert_eq!(class_in_event("activewindow>>,"), Some(""));
        assert_eq!(class_in_event("workspace>>2"), None);
    }

    #[test]
    fn reads_the_class_from_an_activewindow_reply() {
        let reply = "Window 55d2c8678d10 -> magicbar:\n\tmapped: 1\n\tclass: kitty\n\ttitle: magicbar\n";
        assert_eq!(class_in_reply(reply), "kitty");
        assert_eq!(class_in_reply("Invalid"), "");
    }
}
