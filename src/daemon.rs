use std::collections::VecDeque;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::actions::Keyboard;
use crate::config::Config;
use crate::input;
use crate::layout::{Action, Item, Key, Layout, Slider};
use crate::media::{self, NowPlaying};
use crate::render::{keys, media as scrubber, slider as slider_view, Canvas};
use crate::sliders;
use crate::state::{self, Window};
use crate::transport::drm::DrmDisplay;
use crate::transport::Display;

// How often the loop wakes without an event, to run its timers.
const TICK: Duration = Duration::from_millis(50);
// A finger on a slider key starts sliding once it has moved this far, as a
// share of the bar's width, or has rested this long.
const SLIDE_AFTER: f64 = 0.004;
const HOLD_AFTER: Duration = Duration::from_millis(250);
// How long a tapped-open slider stays after the last touch.
const SLIDER_STAYS: Duration = Duration::from_secs(1);
// Everything the daemon reacts to. Each source runs on its own thread
// and sends into the one channel the daemon reads.
#[derive(Debug)]
pub enum Event {
    // A finger on the bar: 0.0 at the left edge, 1.0 at the right.
    Touch(f64),
    // The finger lifted.
    Release,
    // Another window took focus, or the focused window's title changed.
    Focus(Window),
    // What the music or video player is doing, or None when nothing plays.
    Media(Option<NowPlaying>),
    ConfigChanged,
}

// A finger holding a slider key, ready to slide.
struct Hold {
    kind: Slider,
    since: Instant,
    start_x: f64,
    start_value: f64,
    sliding: bool,
}

// A slider shown on the bar.
struct Open {
    kind: Slider,
    value: f64,
    // Its left edge in pixels.
    left: f64,
    // When it closes, unless touched again.
    until: Instant,
}

impl Open {
    // Opens with the knob under the finger at x, kept inside the bar.
    fn under(kind: Slider, value: f64, x: f64, width: f64, until: Instant) -> Open {
        let left = (x - slider_view::knob_at(value)).clamp(0.0, width - slider_view::WIDTH);
        Open { kind, value, left, until }
    }

    fn item(&self) -> Item {
        Item::Slider { kind: self.kind, value: self.value, left: self.left }
    }
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

    let mut window = Window::default();
    let mut expanded = false;
    let mut now: Option<NowPlaying> = None;
    // When `now`'s position was last set, so it can be moved on with time.
    let mut clock = Instant::now();
    let mut layout = config.layout_for(&window.class, expanded, false, None);
    let mut spans = keys::spans(&layout, width);
    let mut pressed = None;
    // Where along the media bar the finger is, while it is being dragged.
    let mut drag = None;
    let mut hold: Option<Hold> = None;
    let mut open: Option<Open> = None;
    // The last time the player or a slider was told a new value.
    let mut last_set = Instant::now();
    // Events set aside while skipping ahead through queued touches.
    let mut pending = VecDeque::new();
    draw(&mut display, &layout, pressed, None)?;

    loop {
        let mut redraw = false;
        let mut event = match pending.pop_front() {
            Some(event) => Some(event),
            None => match rx.recv_timeout(TICK) {
                Ok(event) => Some(event),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => anyhow::bail!("every event source has stopped"),
            },
        };
        // Touches can arrive faster than the bar is drawn; only the newest one matters.
        if let Some(Event::Touch(_)) = event {
            while let Ok(next) = rx.try_recv() {
                match next {
                    Event::Touch(_) => event = Some(next),
                    other => {
                        pending.push_back(other);
                        break;
                    }
                }
            }
        }
        match event {
            Some(Event::Touch(x)) => {
                let next = keys::key_at(&spans, x * width);
                if next != pressed {
                    pressed = next;
                    drag = None;
                    redraw = true;
                }
                let item = next.and_then(|i| layout.items().nth(i));
                // A finger landing on a slider key may be about to slide.
                if let (None, Some(Item::Key(key))) = (&hold, item) {
                    if let Action::Slider(kind) = key.action {
                        let start_value = sliders::get(kind).unwrap_or(0.5);
                        hold = Some(Hold { kind, since: Instant::now(), start_x: x, start_value, sliding: false });
                    }
                }
                if let Some(hold) = &mut hold {
                    if hold.sliding || (x - hold.start_x).abs() > SLIDE_AFTER {
                        hold.sliding = true;
                        let slider = open.get_or_insert_with(|| {
                            Open::under(hold.kind, hold.start_value, hold.start_x * width, width, Instant::now())
                        });
                        slide(slider, slider_view::hit(x * width - slider.left), &mut last_set);
                        redraw = true;
                    }
                }
                // Dragging a tapped-open slider's knob.
                if let (Some(Item::Slider { .. }), Some(slider), None) = (item, &mut open, &hold) {
                    slide(slider, slider_view::hit(x * width - slider.left), &mut last_set);
                    slider.until = Instant::now() + SLIDER_STAYS;
                    redraw = true;
                }
                if let (Some(i), Some(Item::Media)) = (next, item) {
                    let (left, right) = spans[i];
                    if let scrubber::Hit::Bar(fraction) = scrubber::hit(x * width - left, right - left) {
                        if drag != Some(fraction) {
                            drag = Some(fraction);
                            redraw = true;
                        }
                        if last_set.elapsed() > Duration::from_millis(100) {
                            last_set = Instant::now();
                            clock = Instant::now();
                            seek(&mut now, fraction);
                        }
                    }
                }
            }
            Some(Event::Release) => {
                let item = pressed.take().and_then(|i| layout.items().nth(i));
                redraw = true;
                if let Some(hold) = hold.take() {
                    // Lifting after a slide closes the slider; a plain tap opens it.
                    open = match (hold.sliding, open.take()) {
                        (true, Some(slider)) => {
                            sliders::set(slider.kind, slider.value);
                            None
                        }
                        _ => {
                            let until = Instant::now() + SLIDER_STAYS;
                            Some(Open::under(hold.kind, hold.start_value, hold.start_x * width, width, until))
                        }
                    };
                } else {
                    match item {
                        Some(Item::Key(key)) => tap(&mut keyboard, key, &mut expanded)?,
                        Some(Item::Media) => match drag.take() {
                            Some(fraction) => {
                                clock = Instant::now();
                                seek(&mut now, fraction);
                            }
                            None => {
                                if let Err(e) = media::play_pause() {
                                    tracing::warn!("media: {e:#}");
                                }
                            }
                        },
                        Some(Item::Slider { .. }) => {
                            if let Some(slider) = &mut open {
                                sliders::set(slider.kind, slider.value);
                                slider.until = Instant::now() + SLIDER_STAYS;
                            }
                        }
                        None => {}
                    }
                }
            }
            Some(Event::Focus(focused)) => window = focused,
            Some(Event::Media(update)) => {
                now = reconcile(now, update, &mut clock);
                redraw = true;
            }
            Some(Event::ConfigChanged) => match Config::load() {
                Ok(new_config) => config = new_config,
                Err(e) => tracing::error!("config not reloaded: {e:#}"),
            },
            None => {}
        }

        // A finger resting on a slider key long enough starts sliding too.
        if let Some(hold) = &mut hold {
            if !hold.sliding && hold.since.elapsed() > HOLD_AFTER {
                hold.sliding = true;
                open = Some(Open::under(hold.kind, hold.start_value, hold.start_x * width, width, Instant::now()));
                redraw = true;
            }
        }

        // A slider opened by a tap closes after a quiet moment.
        if let Some(slider) = &open {
            if hold.is_none() && pressed.is_none() && Instant::now() > slider.until {
                open = None;
            }
        }

        // Focus, the strip toggle, the player, a slider, or a config save may
        // have changed what to show. The now-playing bar only shows in the
        // player's own window.
        let has_media = match &now {
            Some(now) => now.is_window(&window),
            None => false,
        };
        let wanted = config.layout_for(&window.class, expanded, has_media, open.as_ref().map(Open::item));
        if wanted != layout {
            layout = wanted;
            // A slider's knob moving changes the layout but not where the keys are.
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

// Moves an open slider to a level and tells the system, at most ten times a second.
fn slide(slider: &mut Open, value: f64, last_set: &mut Instant) {
    slider.value = value.clamp(0.0, 1.0);
    if last_set.elapsed() > Duration::from_millis(100) {
        *last_set = Instant::now();
        sliders::set(slider.kind, slider.value);
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
        // Sliders are handled by the touch loop.
        Action::Slider(_) => {}
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
