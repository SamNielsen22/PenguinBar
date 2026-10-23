use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::actions::Keyboard;
use crate::config::Config;
use crate::input;
use crate::layout::{Action, Item, Key, Layout};
use crate::media::{self, NowPlaying};
use crate::render::{keys, media as scrubber, Canvas};
use crate::state::{self, Window, Workspaces};
use crate::transport::drm::DrmDisplay;
use crate::transport::Display;

// The bar reports every 16 ms while touched, so this much silence means the finger lifted.
const LIFT_AFTER: Duration = Duration::from_millis(80);
// How often the active workspace's preview is refreshed, how often all of
// them are, and the delay after a switch for the animation to finish.
const PREVIEW_ACTIVE_EVERY: Duration = Duration::from_secs(2);
const PREVIEW_ALL_EVERY: Duration = Duration::from_secs(20);
const PREVIEW_AFTER_SWITCH: Duration = Duration::from_millis(400);

// Everything the daemon reacts to. Each source runs on its own thread
// and sends into the one channel the daemon reads.
#[derive(Debug)]
pub enum Event {
    // A finger on the bar: 0.0 at the left edge, 1.0 at the right.
    Touch(f64),
    // The finger lifted. Sent by the daemon when the touches stop.
    Release,
    // Another window took focus, or the focused window's title changed.
    Focus(Window),
    // What the music or video player is doing, or None when nothing plays.
    Media(Option<NowPlaying>),
    Workspaces(Workspaces),
    // A small screenshot of a workspace, as PNG.
    Preview(i32, Vec<u8>),
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
    let _watcher = watch_config(tx.clone())?;

    let mut window = Window::default();
    let mut expanded = false;
    let mut now: Option<NowPlaying> = None;
    let mut workspaces = Workspaces::default();
    let mut next_preview = Instant::now() + PREVIEW_AFTER_SWITCH;
    let mut next_preview_all = Instant::now() + PREVIEW_AFTER_SWITCH;
    // When `now`'s position was last set, so it can be moved on with time.
    let mut clock = Instant::now();
    let mut layout = config.layout_for(&window.class, expanded, false, &workspaces);
    let mut spans = keys::spans(&layout, width);
    let mut pressed = None;
    // Where along the media bar the finger is, while it is being dragged.
    let mut drag = None;
    let mut last_seek = Instant::now();
    draw(&mut display, &layout, pressed, None)?;

    loop {
        let mut redraw = false;
        // Refresh the workspace previews.
        if Instant::now() >= next_preview_all {
            next_preview_all = Instant::now() + PREVIEW_ALL_EVERY;
            next_preview = Instant::now() + PREVIEW_ACTIVE_EVERY;
            state::spawn_previews(workspaces.all_shots(), tx.clone());
        } else if Instant::now() >= next_preview {
            next_preview = Instant::now() + PREVIEW_ACTIVE_EVERY;
            state::spawn_previews(workspaces.active_shot(), tx.clone());
        }
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
                // A finger on the media bar scrubs.
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
                        _ => {}
                    }
                    redraw = true;
                }
            }
            Event::Focus(focused) => window = focused,
            Event::Media(update) => {
                now = reconcile(now, update, &mut clock);
                redraw = true;
            }
            Event::Workspaces(mut update) => {
                update.previews = std::mem::take(&mut workspaces.previews);
                workspaces = update;
                next_preview = Instant::now() + PREVIEW_AFTER_SWITCH;
            }
            Event::Preview(id, png) => {
                tracing::debug!("preview of workspace {id}, {} bytes", png.len());
                workspaces.previews.insert(id, png);
            }
            Event::ConfigChanged => match Config::load() {
                Ok(new_config) => config = new_config,
                Err(e) => tracing::error!("config not reloaded: {e:#}"),
            },
        }

        // Focus, the strip toggle, the player, or a config save may have changed what to show.
        // The now-playing bar only shows in the player's own window.
        let has_media = match &now {
            Some(now) => now.is_window(&window),
            None => false,
        };
        let wanted = config.layout_for(&window.class, expanded, has_media, &workspaces);
        if wanted != layout {
            layout = wanted;
            // A new preview picture changes the layout but not where the keys are,
            // so a finger already on a key keeps its key.
            let new_spans = keys::spans(&layout, width);
            if new_spans != spans {
                spans = new_spans;
                pressed = None;
            }
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

// Merges the player's latest report. A report of zero position and length
// on the same track is stale, and the current count is kept instead.
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

// Jumps the player to a point along the track, 0.0 to 1.0. The bar shows
// the new position at once; the player is told on a thread.
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
        Action::Workspace(id) => {
            if let Err(e) = state::switch_workspace(*id) {
                tracing::warn!("workspace: {e:#}");
            }
        }
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
