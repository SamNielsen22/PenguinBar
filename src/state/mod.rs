pub mod hyprland;

use std::sync::mpsc::Sender;

use crate::daemon::Event;

// The focused window's class, like "firefox", and title.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Window {
    pub class: String,
    pub title: String,
}

// Starts a thread that sends a Focus event each time the focused window
// or its title changes. One function per desktop; the first match wins.
pub fn spawn_watcher(tx: Sender<Event>) -> anyhow::Result<()> {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        return hyprland::spawn_watcher(tx);
    }
    tracing::warn!("no supported desktop found, so the bar will never show app keys");
    Ok(())
}
