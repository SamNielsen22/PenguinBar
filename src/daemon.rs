use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::Context;
use evdev::KeyCode;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::actions::Keyboard;
use crate::config::Config;
use crate::input;
use crate::layout::{Action, Item, Layout, Slider};
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

// Something the bar wants done outside itself. The run loop does it.
#[derive(Debug, PartialEq)]
enum Effect {
    Keys(Vec<KeyCode>),
    Command(String),
    SetLevel(Slider, f64),
    PlayPause(String),
    Seek(String, Duration),
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
    width: f64,
    // Reads a slider's current level when a finger lands on its key.
    read_level: fn(Slider) -> f64,
    effects: Vec<Effect>,
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
    let mut keyboard = Keyboard::new()?;
    let mut display = DrmDisplay::open()?;
    let (width, _) = display.size();

    let (tx, rx) = mpsc::channel();
    input::spawn_reader(input::find_touch_device()?, tx.clone())?;
    state::spawn_watcher(tx.clone())?;
    media::spawn_watcher(tx.clone());
    let _watcher = watch_config(tx)?;

    let mut bar = Bar::new(config, width as f64, read_level);
    bar.draw(&mut display)?;
    // Other events pulled out of the queue while skipping old touches.
    let mut pending = VecDeque::new();

    loop {
        let timed = bar.hold.is_some() || bar.open.is_some();
        let event = next_event(&rx, &mut pending, timed)?;
        let redraw = bar.handle(event);
        for effect in bar.effects.drain(..) {
            perform(effect, &mut keyboard)?;
        }
        if redraw {
            bar.draw(&mut display)?;
        }
    }
}

fn read_level(kind: Slider) -> f64 {
    sliders::get(kind).unwrap_or(0.5)
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

// Does what the bar asked for. Anything slow runs on a thread so the bar never waits.
fn perform(effect: Effect, keyboard: &mut Keyboard) -> anyhow::Result<()> {
    match effect {
        Effect::Keys(keys) => keyboard.tap(&keys)?,
        Effect::Command(command) => {
            let mut child = std::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .spawn()?;
            // Reap it or it becomes a zombie.
            std::thread::spawn(move || child.wait());
        }
        Effect::SetLevel(kind, value) => sliders::set(kind, value),
        Effect::PlayPause(app) => {
            std::thread::spawn(move || {
                if let Err(e) = media::play_pause(&app) {
                    tracing::warn!("media: {e:#}");
                }
            });
        }
        Effect::Seek(app, position) => {
            std::thread::spawn(move || {
                if let Err(e) = media::seek_to(&app, position) {
                    tracing::warn!("media: {e:#}");
                }
            });
        }
    }
    Ok(())
}

impl Bar {
    fn new(config: Config, width: f64, read_level: fn(Slider) -> f64) -> Bar {
        let layout = config.layout_for("", false, false, None);
        let spans = keys::spans(&layout, width);
        Bar {
            config,
            width,
            read_level,
            effects: Vec::new(),
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

    // Reacts to one event, or to the timers when there is none. Returns whether to redraw.
    fn handle(&mut self, event: Option<Event>) -> bool {
        let mut changed = event.is_some();
        let mut redraw = false;
        match event {
            Some(Event::Touch(x)) => redraw = self.touch(x),
            Some(Event::Release) => {
                self.release();
                redraw = true;
            }
            Some(Event::Focus(window)) => {
                self.window = window;
                self.update_player();
            }
            Some(Event::Media(players)) => {
                self.players = players;
                self.update_player();
                redraw = true;
            }
            Some(Event::ConfigChanged) => match Config::load() {
                Ok(config) => self.config = config,
                Err(e) => tracing::error!("config not reloaded: {e:#}"),
            },
            None => {}
        }
        if self.run_timers() {
            changed = true;
        }
        if changed && self.update_layout() {
            redraw = true;
        }
        redraw
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
                    start_value: (self.read_level)(kind),
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
                slide(
                    slider,
                    x * self.width,
                    &mut self.last_set,
                    &mut self.effects,
                );
                redraw = true;
            }
        }

        // Dragging a slider that a tap opened.
        if let (Some(Item::Slider { .. }), Some(slider), None) = (&item, &mut self.open, &self.hold)
        {
            slide(
                slider,
                x * self.width,
                &mut self.last_set,
                &mut self.effects,
            );
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
                    self.seek(fraction);
                }
            }
        }
        redraw
    }

    fn release(&mut self) {
        let pressed = self.pressed.take();
        if let Some(hold) = self.hold.take() {
            // Slide then lift closes it. A plain tap opens it.
            self.open = match (hold.sliding, self.open.take()) {
                (true, Some(slider)) => {
                    self.effects
                        .push(Effect::SetLevel(slider.kind, slider.value));
                    None
                }
                _ => Some(hold.open(self.width, Instant::now() + SLIDER_STAYS)),
            };
            return;
        }
        let i = match pressed {
            Some(i) => i,
            None => return,
        };
        let item = self.layout.items().nth(i).cloned();
        match item {
            Some(Item::Key(key)) => {
                tracing::debug!("tap {}", key.label);
                match key.action {
                    Action::Key(keys) => self.effects.push(Effect::Keys(keys)),
                    Action::Command(command) => self.effects.push(Effect::Command(command)),
                    Action::ToggleStrip => self.expanded = !self.expanded,
                    Action::Slider(_) => {}
                }
            }
            Some(Item::Media) => {
                let (left, right) = self.spans[i];
                match scrubber::hit(self.last_x * self.width - left, right - left) {
                    scrubber::Hit::Button => self.play_pause(),
                    scrubber::Hit::Bar(fraction) => {
                        let fraction = self.drag.take().unwrap_or(fraction);
                        self.seek(fraction);
                    }
                }
            }
            Some(Item::Slider { .. }) => {
                if let Some(slider) = &mut self.open {
                    self.effects
                        .push(Effect::SetLevel(slider.kind, slider.value));
                    slider.until = Instant::now() + SLIDER_STAYS;
                }
            }
            None => {}
        }
    }

    // Flips the icon right away instead of waiting for the player to report back.
    fn play_pause(&mut self) {
        self.clock = Instant::now();
        if let Some(now) = &mut self.now {
            now.playing = !now.playing;
            self.effects.push(Effect::PlayPause(now.app.clone()));
        }
    }

    // Moves the bar right away instead of waiting for the player to report back.
    fn seek(&mut self, fraction: f64) {
        self.clock = Instant::now();
        if let Some(now) = &mut self.now {
            *now = now.at(fraction);
            self.effects
                .push(Effect::Seek(now.app.clone(), now.position));
        }
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

// Moves the knob under the finger at x pixels and asks for the new level, at most every SET_EVERY.
fn slide(slider: &mut Open, x: f64, last_set: &mut Instant, effects: &mut Vec<Effect>) {
    slider.value = slider_view::hit(x - slider.left);
    if last_set.elapsed() > SET_EVERY {
        *last_set = Instant::now();
        effects.push(Effect::SetLevel(slider.kind, slider.value));
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

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
        esc = { label = "esc", key = "KEY_ESC" }

        [strip]
        collapsed = [{ label = "b", slider = "brightness" }]
        expanded = [[{ label = "up", key = "KEY_VOLUMEUP" }]]

        [layouts.default]
        groups = [[{ widget = "media" }]]

        [layouts.test]
        apps = ["test"]
        groups = [
            [{ label = "a", key = "KEY_A" }, { label = "copy", key = "KEY_LEFTCTRL+KEY_C" }],
            [{ label = "run", command = "true" }],
        ]
    "#;

    // A bar with the "test" app focused. Every slider starts at 0.4.
    fn bar() -> Bar {
        let mut bar = Bar::new(Config::parse(CONFIG).unwrap(), 2170.0, |_| 0.4);
        bar.handle(Some(Event::Focus(Window {
            class: "test".to_string(),
            title: String::new(),
        })));
        bar
    }

    // The touch position of the middle of the key with this label.
    fn on(bar: &Bar, label: &str) -> f64 {
        let i = bar
            .layout
            .items()
            .position(|item| matches!(item, Item::Key(key) if key.label == label))
            .unwrap();
        let (left, right) = bar.spans[i];
        (left + right) / 2.0 / bar.width
    }

    fn shows(bar: &Bar, label: &str) -> bool {
        bar.layout
            .items()
            .any(|item| matches!(item, Item::Key(key) if key.label == label))
    }

    fn media_span(bar: &Bar) -> (f64, f64) {
        let i = bar
            .layout
            .items()
            .position(|item| *item == Item::Media)
            .unwrap();
        bar.spans[i]
    }

    fn lift(bar: &mut Bar) -> Vec<Effect> {
        bar.handle(Some(Event::Release));
        std::mem::take(&mut bar.effects)
    }

    fn tap(bar: &mut Bar, x: f64) -> Vec<Effect> {
        bar.handle(Some(Event::Touch(x)));
        lift(bar)
    }

    fn start_playing(bar: &mut Bar) {
        bar.handle(Some(Event::Media(vec![NowPlaying {
            app: "test".to_string(),
            title: String::new(),
            playing: true,
            position: Duration::from_secs(10),
            length: Duration::from_secs(100),
        }])));
    }

    #[test]
    fn a_key_fires_when_the_finger_lifts() {
        let mut bar = bar();
        let x = on(&bar, "copy");
        bar.handle(Some(Event::Touch(x)));
        assert!(bar.effects.is_empty());
        assert_eq!(
            lift(&mut bar),
            vec![Effect::Keys(vec![KeyCode::KEY_LEFTCTRL, KeyCode::KEY_C])]
        );
    }

    #[test]
    fn the_key_under_the_finger_when_it_lifts_is_the_one_that_fires() {
        let mut bar = bar();
        let (a, run) = (on(&bar, "a"), on(&bar, "run"));
        bar.handle(Some(Event::Touch(a)));
        bar.handle(Some(Event::Touch(run)));
        assert_eq!(lift(&mut bar), vec![Effect::Command("true".to_string())]);
    }

    #[test]
    fn lifting_in_a_gap_does_nothing() {
        let mut bar = bar();
        let gap = (bar.spans[0].1 + 10.0) / bar.width;
        assert!(tap(&mut bar, gap).is_empty());
    }

    #[test]
    fn the_chevron_expands_the_strip_and_the_x_closes_it() {
        let mut bar = bar();
        let chevron = on(&bar, "\u{f104}");
        tap(&mut bar, chevron);
        assert!(shows(&bar, "up"));
        assert!(!shows(&bar, "a"));

        let close = on(&bar, "\u{f0159}");
        tap(&mut bar, close);
        assert!(shows(&bar, "a"));
        assert!(!shows(&bar, "up"));
    }

    #[test]
    fn a_tapped_slider_opens_sets_the_level_when_touched_and_closes_after_a_moment() {
        let mut bar = bar();
        let key = on(&bar, "b");
        assert!(tap(&mut bar, key).is_empty());
        assert!(bar.open.is_some());

        let (left, right) = *bar.spans.last().unwrap();
        let middle = (left + right) / 2.0 / bar.width;
        let effects = tap(&mut bar, middle);
        let value = bar.open.as_ref().unwrap().value;
        assert_eq!(
            effects.last(),
            Some(&Effect::SetLevel(Slider::Brightness, value))
        );

        bar.open.as_mut().unwrap().until = Instant::now() - Duration::from_millis(1);
        bar.handle(None);
        assert!(bar.open.is_none());
        assert!(shows(&bar, "b"));
    }

    #[test]
    fn holding_a_slider_key_and_sliding_sets_the_level_on_lift() {
        let mut bar = bar();
        let key = on(&bar, "b");
        bar.handle(Some(Event::Touch(key)));
        bar.handle(Some(Event::Touch(key - 0.05)));
        let first = bar.open.as_ref().unwrap().value;
        bar.handle(Some(Event::Touch(key - 0.1)));
        let second = bar.open.as_ref().unwrap().value;
        assert!(second < first);

        bar.effects.clear();
        assert_eq!(
            lift(&mut bar),
            vec![Effect::SetLevel(Slider::Brightness, second)]
        );
        assert!(bar.open.is_none());
    }

    #[test]
    fn resting_on_a_slider_key_opens_the_slider_after_a_moment() {
        let mut bar = bar();
        let key = on(&bar, "b");
        bar.handle(Some(Event::Touch(key)));
        assert!(bar.open.is_none());

        bar.hold.as_mut().unwrap().since = Instant::now() - HOLD_AFTER * 2;
        bar.handle(None);
        assert!(bar.open.is_some());
    }

    #[test]
    fn the_media_button_flips_the_icon_at_once_and_tells_the_player() {
        let mut bar = bar();
        start_playing(&mut bar);
        let (left, _) = media_span(&bar);
        let button = (left + 10.0) / bar.width;
        assert_eq!(
            tap(&mut bar, button),
            vec![Effect::PlayPause("test".to_string())]
        );
        assert!(!bar.now.as_ref().unwrap().playing);
    }

    #[test]
    fn dragging_the_media_bar_seeks_to_where_the_finger_lifts() {
        let mut bar = bar();
        start_playing(&mut bar);
        let (left, right) = media_span(&bar);
        bar.handle(Some(Event::Touch((left + 300.0) / bar.width)));
        bar.handle(Some(Event::Touch((right - 1.0) / bar.width)));

        bar.effects.clear();
        assert_eq!(
            lift(&mut bar),
            vec![Effect::Seek("test".to_string(), Duration::from_secs(100))]
        );
    }
}
