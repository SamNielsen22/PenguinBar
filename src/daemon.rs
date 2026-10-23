use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::actions::Keyboard;
use crate::config::Config;
use crate::input;
use crate::layout::{Action, Item, Key, Layout};
use crate::media::{self, NowPlaying};
use crate::render::{keys, media as scrubber, Canvas};
use crate::state;
use crate::transport::drm::DrmDisplay;
use crate::transport::Display;

// The bar reports every 16 ms while touched, so this much silence means the finger lifted.
const LIFT_AFTER: Duration = Duration::from_millis(80);

// Everything the daemon reacts to. Each source runs on its own thread
// and sends into the one channel the daemon reads.
#[derive(Debug)]
pub enum Event {
    // A finger on the bar: 0.0 at the left edge, 1.0 at the right.
    Touch(f64),
    // The finger lifted. The bar does not say so; the daemon sends this
    // itself when the touches stop.
    Release,
    // Another window took focus. This is its class, like "firefox".
    Focus(String),
    // What the music or video player is doing, or None when nothing plays.
    Media(Option<NowPlaying>),
    ConfigChanged,
}

pub fn run() -> anyhow::Result<()> {
    let mut config = Config::load()?;
    let mut keyboard = Keyboard::new()?;
    let mut display = DrmDisplay::open()?;
    let (width, _) = display.size();
    let width = width as f64;

    let (tx, rx) = mpsc::channel();
    input::spawn_reader(input::find_touch_device()?, tx.clone())?;
    state::spawn_watcher(tx.clone())?;
    media::spawn_watcher(tx.clone());
    let _watcher = watch_config(tx)?;

    let mut class = String::new();
    let mut expanded = false;
    let mut now: Option<NowPlaying> = None;
    // When `now`'s position was last set, so it can be moved on with time.
    let mut clock = Instant::now();
    let mut layout = config.layout_for(&class, expanded, false);
    let mut spans = keys::spans(&layout, width);
    let mut pressed = None;
    // Where along the media bar the finger is, while it is being dragged.
    let mut drag = None;
    let mut last_seek = Instant::now();
    draw(&mut display, &layout, pressed, None)?;

    loop {
        let mut redraw = false;
        let event = match rx.recv_timeout(LIFT_AFTER) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) if pressed.is_some() => Event::Release,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => anyhow::bail!("every event source has stopped"),
        };
        match event {
            Event::Touch(x) => {
                let next = keys::key_at(&spans, x * width);
                if next != pressed {
                    pressed = next;
                    drag = None;
                    redraw = true;
                }
                // A finger on the media bar scrubs instead of pressing.
                if let Some(i) = next {
                    if layout.items().nth(i) == Some(&Item::Media) {
                        let (left, right) = spans[i];
                        if let scrubber::Hit::Bar(fraction) = scrubber::hit(x * width - left, right - left) {
                            if drag != Some(fraction) {
                                drag = Some(fraction);
                                redraw = true;
                            }
                            // Scrub live as the finger moves, but not more than ten times a second.
                            if last_seek.elapsed() > Duration::from_millis(100) {
                                last_seek = Instant::now();
                                clock = Instant::now();
                                seek(&mut now, fraction);
                            }
                        }
                    }
                }
            }
            Event::Release => {
                if let Some(i) = pressed.take() {
                    match layout.items().nth(i) {
                        Some(Item::Key(key)) => tap(&mut keyboard, key, &mut expanded)?,
                        Some(Item::Media) => {
                            match drag.take() {
                                Some(fraction) => {
                                    clock = Instant::now();
                                    seek(&mut now, fraction);
                                }
                                None => {
                                    if let Err(e) = media::play_pause() {
                                        tracing::warn!("media: {e:#}");
                                    }
                                }
                            }
                        }
                        None => {}
                    }
                    redraw = true;
                }
            }
            Event::Focus(new_class) => class = new_class,
            Event::Media(update) => {
                now = reconcile(now, update, &mut clock);
                redraw = true;
            }
            Event::ConfigChanged => match Config::load() {
                Ok(new_config) => config = new_config,
                Err(e) => tracing::error!("config not reloaded: {e:#}"),
            },
        }

        // Focus, the strip toggle, the player, or a config save may have changed what to show.
        // Like Apple, the now-playing bar only shows in the player's own window.
        let has_media = match &now {
            Some(now) => now.is_app(&class),
            None => false,
        };
        let wanted = config.layout_for(&class, expanded, has_media);
        if wanted != layout {
            layout = wanted;
            spans = keys::spans(&layout, width);
            pressed = None;
            redraw = true;
        }
        if redraw {
            // While dragging, show the bar where the finger is.
            let shown = match (&now, drag) {
                (Some(now), Some(fraction)) => Some(now.at(fraction)),
                _ => now.clone(),
            };
            draw(&mut display, &layout, pressed, shown.as_ref())?;
        }
    }
}

// Takes the player's latest report. After a seek Firefox reports a position
// and length of zero until the page says otherwise, so keep counting on our own.
fn reconcile(ours: Option<NowPlaying>, theirs: Option<NowPlaying>, clock: &mut Instant) -> Option<NowPlaying> {
    let elapsed = clock.elapsed();
    *clock = Instant::now();
    let (mut ours, theirs) = match (ours, theirs) {
        (Some(ours), Some(theirs)) => (ours, theirs),
        (_, theirs) => return theirs,
    };
    if ours.playing {
        ours.position += elapsed;
    }
    let same_track = theirs.length.is_zero() || theirs.length == ours.length;
    let stale = theirs.position.is_zero() && !ours.position.is_zero() && same_track;
    if stale {
        Some(NowPlaying { playing: theirs.playing, ..ours })
    } else {
        Some(theirs)
    }
}

// Jumps the player to a point along the track, 0.0 to 1.0, and shows it there
// right away. The player is told on a thread, since a browser can take a
// while to seek and the bar must keep following the finger meanwhile.
fn seek(now: &mut Option<NowPlaying>, fraction: f64) {
    let target = match now {
        Some(now) => now.at(fraction),
        None => return,
    };
    *now = Some(target.clone());
    std::thread::spawn(move || {
        if let Err(e) = media::seek_to(target.position) {
            tracing::warn!("media: {e:#}");
        }
    });
}

// Does what the key says: press a key, run a command, or toggle the strip.
fn tap(keyboard: &mut Keyboard, key: &Key, expanded: &mut bool) -> anyhow::Result<()> {
    tracing::debug!("tap {}", key.label);
    match &key.action {
        Action::Key(code) => keyboard.tap(*code)?,
        Action::Command(command) => {
            let mut child = std::process::Command::new("sh").arg("-c").arg(command).spawn()?;
            // Wait on a thread so the finished process is cleaned up.
            std::thread::spawn(move || child.wait());
        }
        Action::ToggleStrip => *expanded = !*expanded,
    }
    Ok(())
}

// Sends ConfigChanged when the config file is saved. Drop the watcher and it stops.
fn watch_config(tx: Sender<Event>) -> anyhow::Result<Option<RecommendedWatcher>> {
    let dir = Config::dir();
    if !dir.exists() {
        return Ok(None);
    }
    let path = Config::path();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event {
            if event.paths.contains(&path) {
                let _ = tx.send(Event::ConfigChanged);
            }
        }
    })?;
    // Editors save by writing a new file and renaming it, so watch the folder.
    watcher.watch(&dir, RecursiveMode::NonRecursive)?;
    Ok(Some(watcher))
}

fn draw(
    display: &mut DrmDisplay,
    layout: &Layout,
    pressed: Option<usize>,
    now: Option<&NowPlaying>,
) -> anyhow::Result<()> {
    let (width, height) = display.size();
    let mut canvas = Canvas::new(width, height)?;
    keys::draw_row(&mut canvas, layout, pressed, now)?;
    display.present(&canvas)
}
