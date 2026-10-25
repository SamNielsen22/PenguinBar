use std::sync::mpsc::Sender;
use std::time::Duration;

use anyhow::Context;
use mpris::{PlaybackStatus, Player, PlayerFinder};

use crate::daemon::Event;
use crate::state::Window;

// position is whole seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct NowPlaying {
    pub app: String,
    pub title: String,
    pub playing: bool,
    pub position: Duration,
    pub length: Duration,
}

impl NowPlaying {
    // The track title has to be in the window title too, otherwise every browser tab would count.
    // A title that is just the app name, like Spotify's while paused, counts too.
    pub fn is_window(&self, window: &Window) -> bool {
        let class = window.class.to_lowercase();
        let title = window.title.to_lowercase();
        let same_app = class.contains(&self.app) || self.app.contains(&class);
        let names_track = self.title.is_empty() || window.title.contains(&self.title);
        same_app && (names_track || title.starts_with(&self.app))
    }

    pub fn at(&self, fraction: f64) -> NowPlaying {
        let position = self.length.mul_f64(fraction.clamp(0.0, 1.0));
        NowPlaying {
            position,
            ..self.clone()
        }
    }
}

// Lowercase desktop entry, or the bus name if the player has none.
fn app_of(player: &Player) -> anyhow::Result<String> {
    let app = match player.get_desktop_entry()? {
        Some(entry) => entry,
        None => player.bus_name_trimmed().to_string(),
    };
    Ok(app.to_lowercase())
}

// The player that says it is this app.
fn player(app: &str) -> anyhow::Result<Player> {
    for player in PlayerFinder::new()?.find_all()? {
        if app_of(&player)? == app {
            return Ok(player);
        }
    }
    anyhow::bail!("{app} has no player")
}

fn now_playing(player: &Player) -> anyhow::Result<Option<NowPlaying>> {
    let status = player.get_playback_status()?;
    if status == PlaybackStatus::Stopped {
        return Ok(None);
    }
    let position = Duration::from_secs(player.get_position()?.as_secs());
    let metadata = player.get_metadata()?;
    Ok(Some(NowPlaying {
        app: app_of(player)?,
        title: metadata.title().unwrap_or_default().to_string(),
        playing: status == PlaybackStatus::Playing,
        position,
        length: metadata.length().unwrap_or_default(),
    }))
}

// Every player that is playing or paused.
fn all_playing(finder: &PlayerFinder) -> Vec<NowPlaying> {
    let mut all = Vec::new();
    for player in finder.find_all().unwrap_or_default() {
        if let Ok(Some(now)) = now_playing(&player) {
            all.push(now);
        }
    }
    all
}

// The player shown in this window, if one is playing or paused there.
pub fn for_window(players: &[NowPlaying], window: &Window) -> Option<NowPlaying> {
    players.iter().find(|now| now.is_window(window)).cloned()
}

// Sends on every check while something plays so the daemon can keep its own clock.
pub fn spawn_watcher(tx: Sender<Event>) {
    std::thread::spawn(move || {
        let finder = match PlayerFinder::new() {
            Ok(finder) => finder,
            Err(e) => {
                tracing::warn!("media: {e}");
                return;
            }
        };
        let mut last = Vec::new();
        loop {
            let players = all_playing(&finder);
            let playing = players.iter().any(|now| now.playing);
            if players != last || playing {
                if tx.send(Event::Media(players.clone())).is_err() {
                    return;
                }
                last = players;
            }
            let wait = if playing { 250 } else { 1000 };
            std::thread::sleep(Duration::from_millis(wait));
        }
    });
}

pub fn play_pause(app: &str) -> anyhow::Result<()> {
    player(app)?.play_pause()?;
    Ok(())
}

pub fn previous(app: &str) -> anyhow::Result<()> {
    player(app)?.previous()?;
    Ok(())
}

pub fn next(app: &str) -> anyhow::Result<()> {
    player(app)?.next()?;
    Ok(())
}

pub fn seek_to(app: &str, position: Duration) -> anyhow::Result<()> {
    let player = player(app)?;
    let track = player
        .get_metadata()?
        .track_id()
        .context("the player has no track id")?;
    // Firefox ignores Seek. SetPosition works.
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
        let video = Window {
            class: "firefox".to_string(),
            title: "Some Video - YouTube — Mozilla Firefox".to_string(),
        };
        let other_tab = Window {
            class: "firefox".to_string(),
            title: "New Tab — Mozilla Firefox".to_string(),
        };
        let terminal = Window {
            class: "kitty".to_string(),
            title: "Some Video".to_string(),
        };
        assert!(now.is_window(&video));
        assert!(!now.is_window(&other_tab));
        assert!(!now.is_window(&terminal));

        let paused = Window {
            class: "Spotify".to_string(),
            title: "Spotify Premium".to_string(),
        };
        let spotify = NowPlaying {
            app: "spotify".to_string(),
            ..now.clone()
        };
        assert!(spotify.is_window(&paused));
        assert!(!now.is_window(&paused));

        let untitled = NowPlaying {
            title: String::new(),
            ..now.clone()
        };
        assert!(untitled.is_window(&other_tab));

        let players = vec![now.clone(), spotify.clone()];
        assert_eq!(for_window(&players, &paused), Some(spotify));
        assert_eq!(for_window(&players, &video), Some(now));
        assert_eq!(for_window(&players, &terminal), None);
    }
}
