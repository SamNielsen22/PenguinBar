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

// Only needed while a slider is held or open.
const TICK: Duration = Duration::from_millis(50);
// Moving this far (fraction of the bar) or holding this long counts as a slide, not a tap.
const SLIDE_AFTER: f64 = 0.004;
const HOLD_AFTER: Duration = Duration::from_millis(250);
const SLIDER_STAYS: Duration = Duration::from_secs(1);
#[derive(Debug)]
pub enum Event {
    // 0.0 is the left edge, 1.0 the right.
    Touch(f64),
    Release,
    Focus(Window),
    // Every player that is playing or paused.
    Media(Vec<NowPlaying>),
    ConfigChanged,
}

struct Hold {
    kind: Slider,
    since: Instant,
    start_x: f64,
    start_value: f64,
    sliding: bool,
}

struct Open {
    kind: Slider,
    value: f64,
    left: f64,
    until: Instant,
}

impl Open {
    fn under(kind: Slider, value: f64, x: f64, width: f64, until: Instant) -> Open {
        let left = (x - slider_view::knob_at(value)).clamp(0.0, width - slider_view::WIDTH);
        Open {
            kind,
            value,
            left,
            until,
        }
    }

    fn item(&self) -> Item {
        Item::Slider {
            kind: self.kind,
            value: self.value,
            left: self.left,
        }
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
    let mut players: Vec<NowPlaying> = Vec::new();
    // The player in the focused window.
    let mut now: Option<NowPlaying> = None;
    // When now.position was last updated. Used to move it on by the time since.
    let mut clock = Instant::now();
    let mut layout = config.layout_for(&window.class, expanded, false, None);
    let mut spans = keys::spans(&layout, width);
    let mut pressed = None;
    let mut last_x = 0.0;
    let mut drag = None;
    let mut hold: Option<Hold> = None;
    let mut open: Option<Open> = None;
    let mut last_set = Instant::now();
    // Other events pulled out of the queue while skipping old touches.
    let mut pending = VecDeque::new();
    draw(&mut display, &layout, pressed, None)?;

    loop {
        let mut redraw = false;
        let timed = hold.is_some() || open.is_some();
        let mut event = match pending.pop_front() {
            Some(event) => Some(event),
            None if timed => match rx.recv_timeout(TICK) {
                Ok(event) => Some(event),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("every event source has stopped")
                }
            },
            None => match rx.recv() {
                Ok(event) => Some(event),
                Err(_) => anyhow::bail!("every event source has stopped"),
            },
        };
        let mut changed = event.is_some();
        // Touches come in faster than we can draw, so skip to the newest.
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
                last_x = x;
                let next = keys::key_at(&spans, x * width);
                if next != pressed {
                    pressed = next;
                    drag = None;
                    redraw = true;
                }
                let item = next.and_then(|i| layout.items().nth(i));
                if let (None, Some(Item::Key(key))) = (&hold, item) {
                    if let Action::Slider(kind) = key.action {
                        let start_value = sliders::get(kind).unwrap_or(0.5);
                        hold = Some(Hold {
                            kind,
                            since: Instant::now(),
                            start_x: x,
                            start_value,
                            sliding: false,
                        });
                    }
                }
                if let Some(hold) = &mut hold {
                    if hold.sliding || (x - hold.start_x).abs() > SLIDE_AFTER {
                        hold.sliding = true;
                        let slider = open.get_or_insert_with(|| {
                            Open::under(
                                hold.kind,
                                hold.start_value,
                                hold.start_x * width,
                                width,
                                Instant::now(),
                            )
                        });
                        slide(
                            slider,
                            slider_view::hit(x * width - slider.left),
                            &mut last_set,
                        );
                        redraw = true;
                    }
                }
                if let (Some(Item::Slider { .. }), Some(slider), None) = (item, &mut open, &hold) {
                    slide(
                        slider,
                        slider_view::hit(x * width - slider.left),
                        &mut last_set,
                    );
                    slider.until = Instant::now() + SLIDER_STAYS;
                    redraw = true;
                }
                if let (Some(i), Some(Item::Media)) = (next, item) {
                    let (left, right) = spans[i];
                    if let scrubber::Hit::Bar(fraction) =
                        scrubber::hit(x * width - left, right - left)
                    {
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
                let i = pressed.take().unwrap_or(usize::MAX);
                let item = layout.items().nth(i);
                let x = last_x;
                redraw = true;
                if let Some(hold) = hold.take() {
                    // Slide then lift closes it. A plain tap opens it.
                    open = match (hold.sliding, open.take()) {
                        (true, Some(slider)) => {
                            sliders::set(slider.kind, slider.value);
                            None
                        }
                        _ => {
                            let until = Instant::now() + SLIDER_STAYS;
                            Some(Open::under(
                                hold.kind,
                                hold.start_value,
                                hold.start_x * width,
                                width,
                                until,
                            ))
                        }
                    };
                } else {
                    match item {
                        Some(Item::Key(key)) => tap(&mut keyboard, key, &mut expanded)?,
                        Some(Item::Media) => {
                            let app = now.as_ref().map(|now| now.app.clone()).unwrap_or_default();
                            let (left, right) = spans[i];
                            let result = match scrubber::hit(x * width - left, right - left) {
                                scrubber::Hit::Previous => media::previous(&app),
                                scrubber::Hit::PlayPause => media::play_pause(&app),
                                scrubber::Hit::Next => media::next(&app),
                                scrubber::Hit::Bar(fraction) => {
                                    clock = Instant::now();
                                    seek(&mut now, drag.take().unwrap_or(fraction));
                                    Ok(())
                                }
                            };
                            if let Err(e) = result {
                                tracing::warn!("media: {e:#}");
                            }
                        }
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
            Some(Event::Focus(focused)) => {
                window = focused;
                now = reconcile(now, media::for_window(&players, &window), &mut clock);
            }
            Some(Event::Media(update)) => {
                players = update;
                now = reconcile(now, media::for_window(&players, &window), &mut clock);
                redraw = true;
            }
            Some(Event::ConfigChanged) => match Config::load() {
                Ok(new_config) => config = new_config,
                Err(e) => tracing::error!("config not reloaded: {e:#}"),
            },
            None => {}
        }

        if let Some(hold) = &mut hold {
            if !hold.sliding && hold.since.elapsed() > HOLD_AFTER {
                hold.sliding = true;
                open = Some(Open::under(
                    hold.kind,
                    hold.start_value,
                    hold.start_x * width,
                    width,
                    Instant::now(),
                ));
                changed = true;
            }
        }

        if let Some(slider) = &open {
            if hold.is_none() && pressed.is_none() && Instant::now() > slider.until {
                open = None;
                changed = true;
            }
        }

        if changed {
            let has_media = now.is_some();
            let wanted = config.layout_for(
                &window.class,
                expanded,
                has_media,
                open.as_ref().map(Open::item),
            );
            if wanted != layout {
                layout = wanted;
                // A moving knob changes the layout but not where the keys are, so keep pressed.
                let new_spans = keys::spans(&layout, width);
                if new_spans != spans {
                    spans = new_spans;
                    pressed = None;
                }
                redraw = true;
            }
        }
        if redraw {
            let shown = match (&now, drag) {
                (Some(now), Some(fraction)) => Some(now.at(fraction)),
                _ => now.clone(),
            };
            draw(&mut display, &layout, pressed, shown.as_ref())?;
        }
    }
}

fn slide(slider: &mut Open, value: f64, last_set: &mut Instant) {
    slider.value = value.clamp(0.0, 1.0);
    if last_set.elapsed() > Duration::from_millis(100) {
        *last_set = Instant::now();
        sliders::set(slider.kind, slider.value);
    }
}

// Right after a seek Firefox reports position 0 and length 0. Ignore it and keep our own count.
fn reconcile(
    ours: Option<NowPlaying>,
    theirs: Option<NowPlaying>,
    clock: &mut Instant,
) -> Option<NowPlaying> {
    let elapsed = clock.elapsed();
    *clock = Instant::now();
    let (mut ours, theirs) = match (ours, theirs) {
        (Some(ours), Some(theirs)) => (ours, theirs),
        (_, theirs) => return theirs,
    };
    if ours.playing {
        ours.position += elapsed;
    }
    let same_track =
        theirs.app == ours.app && (theirs.length.is_zero() || theirs.length == ours.length);
    let stale = theirs.position.is_zero() && !ours.position.is_zero() && same_track;
    if stale {
        Some(NowPlaying {
            playing: theirs.playing,
            ..ours
        })
    } else {
        Some(theirs)
    }
}

fn seek(now: &mut Option<NowPlaying>, fraction: f64) {
    let target = match now {
        Some(now) => now.at(fraction),
        None => return,
    };
    *now = Some(target.clone());
    std::thread::spawn(move || {
        if let Err(e) = media::seek_to(&target.app, target.position) {
            tracing::warn!("media: {e:#}");
        }
    });
}

fn tap(keyboard: &mut Keyboard, key: &Key, expanded: &mut bool) -> anyhow::Result<()> {
    tracing::debug!("tap {}", key.label);
    match &key.action {
        Action::Key(keys) => keyboard.tap(keys)?,
        Action::Command(command) => {
            let mut child = std::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .spawn()?;
            // Reap it or it becomes a zombie.
            std::thread::spawn(move || child.wait());
        }
        Action::ToggleStrip => *expanded = !*expanded,
        Action::Slider(_) => {}
    }
    Ok(())
}

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
    // Editors save a temp file and rename it, so watch the folder instead of the file.
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
