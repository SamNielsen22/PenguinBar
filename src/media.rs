use std::sync::mpsc::Sender;
use std::time::Duration;

use anyhow::Context;
use mpris::{PlaybackStatus, Player, PlayerFinder};

use crate::daemon::Event;
use crate::state::Window;

// What the active player is doing. Position is whole seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct NowPlaying {
    // The player's app name, like "firefox" or "spotify".
    pub app: String,
    // The track or video title, as the player reports it.
    pub title: String,
    pub playing: bool,
    pub position: Duration,
    pub length: Duration,
}

impl NowPlaying {
    // Whether the focused window is where this is playing: the player's app,
    // and a window title that mentions the track, so a browser only counts
    // on the tab with the video.
    pub fn is_window(&self, window: &Window) -> bool {
        let class = window.class.to_lowercase();
        let same_app = class.contains(&self.app) || self.app.contains(&class);
        same_app && (self.title.is_empty() || window.title.contains(&self.title))
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
    let metadata = player.get_metadata()?;
    Ok(Some(NowPlaying {
        app: app.to_lowercase(),
        title: metadata.title().unwrap_or_default().to_string(),
        playing: status == PlaybackStatus::Playing,
        position,
        length: metadata.length().unwrap_or_default(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plays_only_in_the_window_that_shows_the_track() {
        let now = NowPlaying {
            app: "firefox".to_string(),
            title: "Some Video".to_string(),
            playing: true,
            position: Duration::ZERO,
            length: Duration::from_secs(60),
        };
        let video = Window { class: "firefox".to_string(), title: "Some Video - YouTube — Mozilla Firefox".to_string() };
        let other_tab = Window { class: "firefox".to_string(), title: "New Tab — Mozilla Firefox".to_string() };
        let terminal = Window { class: "kitty".to_string(), title: "Some Video".to_string() };
        assert!(now.is_window(&video));
        assert!(!now.is_window(&other_tab));
        assert!(!now.is_window(&terminal));

        let untitled = NowPlaying { title: String::new(), ..now };
        assert!(untitled.is_window(&other_tab));
    }
}
