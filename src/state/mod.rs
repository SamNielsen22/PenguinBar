pub mod hyprland;

use std::sync::mpsc::Sender;

use crate::daemon::Event;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Window {
    pub class: String,
    pub title: String,
}

pub fn spawn_watcher(tx: Sender<Event>) -> anyhow::Result<()> {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        return hyprland::spawn_watcher(tx);
    }
    tracing::warn!("no supported desktop found, so the bar will never show app keys");
    Ok(())
}
