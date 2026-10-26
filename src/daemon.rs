use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::Context;
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

// Only needed while a slider is held or open.
const TICK: Duration = Duration::from_millis(50);
// Moving this far (fraction of the bar) or holding this long counts as a slide, not a tap.
const SLIDE_AFTER: f64 = 0.004;
const HOLD_AFTER: Duration = Duration::from_millis(250);
const SLIDER_STAYS: Duration = Duration::from_secs(1);
// Sliders and seeks tell the system at most this often while dragging.
const SET_EVERY: Duration = Duration::from_millis(100);

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

impl Hold {
    // The slider this hold opens, with the knob under where the finger landed.
    fn open(&self, width: f64, until: Instant) -> Open {
        let x = self.start_x * width;
        let left =
            (x - slider_view::knob_at(self.start_value)).clamp(0.0, width - slider_view::WIDTH);
        Open {
            kind: self.kind,
            value: self.start_value,
            left,
            until,
        }
    }
}

struct Open {
    kind: Slider,
    value: f64,
    left: f64,
    until: Instant,
}

impl Open {
    fn item(&self) -> Item {
        Item::Slider {
            kind: self.kind,
            value: self.value,
            left: self.left,
        }
    }
}

struct Bar {
    config: Config,
    keyboard: Keyboard,
    width: f64,
    window: Window,
    expanded: bool,
    players: Vec<NowPlaying>,
    // The player in the focused window.
    now: Option<NowPlaying>,
    // When now.position was last updated. Used to move it on by the time since.
    clock: Instant,
    layout: Layout,
    spans: keys::Spans,
    pressed: Option<usize>,
    last_x: f64,
    drag: Option<f64>,
    hold: Option<Hold>,
    open: Option<Open>,
    last_set: Instant,
}

pub fn run() -> anyhow::Result<()> {
    let config = Config::load()?;
    let keyboard = Keyboard::new()?;
    let mut display = DrmDisplay::open()?;
    let (width, _) = display.size();

    let (tx, rx) = mpsc::channel();
    input::spawn_reader(input::find_touch_device()?, tx.clone())?;
    state::spawn_watcher(tx.clone())?;
    media::spawn_watcher(tx.clone());
    let _watcher = watch_config(tx)?;

    let mut bar = Bar::new(config, keyboard, width as f64);
    bar.draw(&mut display)?;
    // Other events pulled out of the queue while skipping old touches.
    let mut pending = VecDeque::new();

    loop {
        let timed = bar.hold.is_some() || bar.open.is_some();
        let event = next_event(&rx, &mut pending, timed)?;
        let mut changed = event.is_some();
        let mut redraw = false;
        match event {
            Some(Event::Touch(x)) => redraw = bar.touch(x),
            Some(Event::Release) => {
                bar.release()?;
                redraw = true;
            }
            Some(Event::Focus(window)) => {
                bar.window = window;
                bar.update_player();
            }
            Some(Event::Media(players)) => {
                bar.players = players;
                bar.update_player();
                redraw = true;
            }
            Some(Event::ConfigChanged) => match Config::load() {
                Ok(config) => bar.config = config,
                Err(e) => tracing::error!("config not reloaded: {e:#}"),
            },
            None => {}
        }
        if bar.run_timers() {
            changed = true;
        }
        if changed && bar.update_layout() {
            redraw = true;
        }
        if redraw {
            bar.draw(&mut display)?;
        }
    }
}

// Waits for the next event, or None when a slider's timer is due.
// Touches come in faster than we can draw, so skip to the newest.
fn next_event(
    rx: &Receiver<Event>,
    pending: &mut VecDeque<Event>,
    timed: bool,
) -> anyhow::Result<Option<Event>> {
    let mut event = match pending.pop_front() {
        Some(event) => event,
        None if timed => match rx.recv_timeout(TICK) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => return Ok(None),
            Err(RecvTimeoutError::Disconnected) => anyhow::bail!("every event source has stopped"),
        },
        None => rx.recv().context("every event source has stopped")?,
    };
    if let Event::Touch(_) = event {
        while let Ok(next) = rx.try_recv() {
            match next {
                Event::Touch(_) => event = next,
                other => {
                    pending.push_back(other);
                    break;
                }
            }
        }
    }
    Ok(Some(event))
}

impl Bar {
    fn new(config: Config, keyboard: Keyboard, width: f64) -> Bar {
        let layout = config.layout_for("", false, false, None);
        let spans = keys::spans(&layout, width);
        Bar {
            config,
            keyboard,
            width,
            window: Window::default(),
            expanded: false,
            players: Vec::new(),
            now: None,
            clock: Instant::now(),
            layout,
            spans,
            pressed: None,
            last_x: 0.0,
            drag: None,
            hold: None,
            open: None,
            last_set: Instant::now(),
        }
    }

    // Returns whether the bar needs redrawing.
    fn touch(&mut self, x: f64) -> bool {
        self.last_x = x;
        let mut redraw = false;
        let next = keys::key_at(&self.spans, x * self.width);
        if next != self.pressed {
            self.pressed = next;
            self.drag = None;
            redraw = true;
        }
        let item = next.and_then(|i| self.layout.items().nth(i)).cloned();

        // A finger landing on a slider key may be the start of a slide.
        if let (None, Some(Item::Key(key))) = (&self.hold, &item) {
            if let Action::Slider(kind) = key.action {
                self.hold = Some(Hold {
                    kind,
                    since: Instant::now(),
                    start_x: x,
                    start_value: sliders::get(kind).unwrap_or(0.5),
                    sliding: false,
                });
            }
        }
        if let Some(hold) = &mut self.hold {
            if hold.sliding || (x - hold.start_x).abs() > SLIDE_AFTER {
                hold.sliding = true;
                let slider = self
                    .open
                    .get_or_insert_with(|| hold.open(self.width, Instant::now()));
                slide(slider, x * self.width, &mut self.last_set);
                redraw = true;
            }
        }

        // Dragging a slider that a tap opened.
        if let (Some(Item::Slider { .. }), Some(slider), None) = (&item, &mut self.open, &self.hold)
        {
            slide(slider, x * self.width, &mut self.last_set);
            slider.until = Instant::now() + SLIDER_STAYS;
            redraw = true;
        }

        if let (Some(i), Some(Item::Media)) = (next, &item) {
            let (left, right) = self.spans[i];
            if let scrubber::Hit::Bar(fraction) = scrubber::hit(x * self.width - left, right - left)
            {
                if self.drag != Some(fraction) {
                    self.drag = Some(fraction);
                    redraw = true;
                }
                if self.last_set.elapsed() > SET_EVERY {
                    self.last_set = Instant::now();
                    self.clock = Instant::now();
                    seek(&mut self.now, fraction);
                }
            }
        }
        redraw
    }

    fn release(&mut self) -> anyhow::Result<()> {
        let pressed = self.pressed.take();
        if let Some(hold) = self.hold.take() {
            // Slide then lift closes it. A plain tap opens it.
            self.open = match (hold.sliding, self.open.take()) {
                (true, Some(slider)) => {
                    sliders::set(slider.kind, slider.value);
                    None
                }
                _ => Some(hold.open(self.width, Instant::now() + SLIDER_STAYS)),
            };
            return Ok(());
        }
        let i = match pressed {
            Some(i) => i,
            None => return Ok(()),
        };
        match self.layout.items().nth(i).cloned() {
            Some(Item::Key(key)) => tap(&mut self.keyboard, &key, &mut self.expanded)?,
            Some(Item::Media) => {
                let (left, right) = self.spans[i];
                self.clock = Instant::now();
                match scrubber::hit(self.last_x * self.width - left, right - left) {
                    scrubber::Hit::Button => play_pause(&mut self.now),
                    scrubber::Hit::Bar(fraction) => {
                        seek(&mut self.now, self.drag.take().unwrap_or(fraction))
                    }
                }
            }
            Some(Item::Slider { .. }) => {
                if let Some(slider) = &mut self.open {
                    sliders::set(slider.kind, slider.value);
                    slider.until = Instant::now() + SLIDER_STAYS;
                }
            }
            None => {}
        }
        Ok(())
    }

    fn update_player(&mut self) {
        let theirs = media::for_window(&self.players, &self.window);
        self.now = reconcile(self.now.take(), theirs, &mut self.clock);
    }

    // A long press starts a slide, and a tapped-open slider closes after a quiet moment.
    // Returns whether anything changed.
    fn run_timers(&mut self) -> bool {
        let mut changed = false;
        if let Some(hold) = &mut self.hold {
            if !hold.sliding && hold.since.elapsed() > HOLD_AFTER {
                hold.sliding = true;
                self.open = Some(hold.open(self.width, Instant::now()));
                changed = true;
            }
        }
        if let Some(slider) = &self.open {
            if self.hold.is_none() && self.pressed.is_none() && Instant::now() > slider.until {
                self.open = None;
                changed = true;
            }
        }
        changed
    }

    // Returns whether the layout changed.
    fn update_layout(&mut self) -> bool {
        let slider = self.open.as_ref().map(Open::item);
        let wanted = self.config.layout_for(
            &self.window.class,
            self.expanded,
            self.now.is_some(),
            slider,
        );
        if wanted == self.layout {
            return false;
        }
        self.layout = wanted;
        // A moving knob changes the layout but not where the keys are, so keep pressed.
        let spans = keys::spans(&self.layout, self.width);
        if spans != self.spans {
            self.spans = spans;
            self.pressed = None;
        }
        true
    }

    fn draw(&self, display: &mut DrmDisplay) -> anyhow::Result<()> {
        // While dragging, show the bar where the finger is.
        let shown = match (&self.now, self.drag) {
            (Some(now), Some(fraction)) => Some(now.at(fraction)),
            _ => self.now.clone(),
        };
        let (width, height) = display.size();
        let mut canvas = Canvas::new(width, height)?;
        keys::draw_row(&mut canvas, &self.layout, self.pressed, shown.as_ref())?;
        display.present(&canvas)
    }
}

// Moves the knob under the finger at x pixels and tells the system, at most every SET_EVERY.
fn slide(slider: &mut Open, x: f64, last_set: &mut Instant) {
    slider.value = slider_view::hit(x - slider.left);
    if last_set.elapsed() > SET_EVERY {
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

// Flips the icon right away and tells the player on a thread, so the bar never waits.
fn play_pause(now: &mut Option<NowPlaying>) {
    let now = match now {
        Some(now) => now,
        None => return,
    };
    now.playing = !now.playing;
    let app = now.app.clone();
    std::thread::spawn(move || {
        if let Err(e) = media::play_pause(&app) {
            tracing::warn!("media: {e:#}");
        }
    });
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
