use std::sync::mpsc::Sender;
use std::time::Duration;

use anyhow::Context;
use mpris::{PlaybackStatus, Player, PlayerFinder};

use crate::daemon::Event;

// What the active player is doing. Position is whole seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct NowPlaying {
    // The player's app name, like "firefox" or "spotify".
    pub app: String,
    pub playing: bool,
    pub position: Duration,
    pub length: Duration,
}

impl NowPlaying {
    // Whether this player is the focused window, given its class.
    pub fn is_app(&self, class: &str) -> bool {
        let class = class.to_lowercase();
        class.contains(&self.app) || self.app.contains(&class)
    }

    // The same track at a point along it, 0.0 to 1.0.
    pub fn at(&self, fraction: f64) -> NowPlaying {
        let position = self.length.mul_f64(fraction.clamp(0.0, 1.0));
        NowPlaying { position, ..self.clone() }
    }
}

// Players announce themselves over D-Bus using the MPRIS standard.
fn active_player() -> anyhow::Result<Player> {
    let player = PlayerFinder::new()?.find_active()?;
    Ok(player)
}

fn now_playing(player: &Player) -> anyhow::Result<Option<NowPlaying>> {
    let status = player.get_playback_status()?;
    if status == PlaybackStatus::Stopped {
        return Ok(None);
    }
    let app = match player.get_desktop_entry()? {
        Some(entry) => entry,
        None => player.bus_name_trimmed().to_string(),
    };
    let position = Duration::from_secs(player.get_position()?.as_secs());
    let length = player.get_metadata()?.length().unwrap_or_default();
    Ok(Some(NowPlaying {
        app: app.to_lowercase(),
        playing: status == PlaybackStatus::Playing,
        position,
        length,
    }))
}

// Checks the active player twice a second. Sends a Media event when something
// changed, and on every check while playing so the daemon can keep time itself.
pub fn spawn_watcher(tx: Sender<Event>) {
    std::thread::spawn(move || {
        let mut last = None;
        loop {
            let now = active_player().and_then(|player| now_playing(&player)).unwrap_or(None);
            let playing = matches!(&now, Some(now) if now.playing);
            if now != last || playing {
                if tx.send(Event::Media(now.clone())).is_err() {
                    return;
                }
                last = now;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

pub fn play_pause() -> anyhow::Result<()> {
    active_player()?.play_pause()?;
    Ok(())
}

// Jumps to a point in the track.
pub fn seek_to(position: Duration) -> anyhow::Result<()> {
    let player = active_player()?;
    let track = player.get_metadata()?.track_id().context("the player has no track id")?;
    // SetPosition is the absolute seek. Browsers only forward this one to the page.
    player.set_position(track, &position)?;
    Ok(())
}
