pub mod hyprland;

use std::sync::mpsc::Sender;

use crate::daemon::Event;

// Starts a thread that sends a Focus event each time another window takes focus.
// One function per desktop; the first one that matches wins.
pub fn spawn_watcher(tx: Sender<Event>) -> anyhow::Result<()> {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        return hyprland::spawn_watcher(tx);
    }
    tracing::warn!("no supported desktop found, so the bar will never show app keys");
    Ok(())
}
