use std::sync::mpsc::{self, Sender};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::actions::Keyboard;
use crate::config::Config;
use crate::input;
use crate::layout::{Action, Key, Layout};
use crate::render::{keys, Canvas};
use crate::state;
use crate::transport::drm::DrmDisplay;
use crate::transport::Display;

// Everything the daemon reacts to. Each source runs on its own thread
// and sends into the one channel the daemon reads.
#[derive(Debug)]
pub enum Event {
    // A finger on the bar: 0.0 at the left edge, 1.0 at the right.
    Touch(f64),
    // The finger lifted.
    Release,
    // Another window took focus. This is its class, like "firefox".
    Focus(String),
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
    let _watcher = watch_config(tx)?;

    let mut class = String::new();
    let mut expanded = false;
    let mut layout = config.layout_for(&class, expanded);
    let mut spans = keys::spans(&layout, width);
    let mut pressed = None;
    draw(&mut display, &layout, pressed)?;

    loop {
        let mut redraw = false;
        match rx.recv()? {
            Event::Touch(x) => {
                let next = keys::key_at(&spans, x * width);
                if next != pressed {
                    pressed = next;
                    redraw = true;
                }
            }
            Event::Release => {
                if let Some(i) = pressed.take() {
                    if let Some(key) = layout.keys().nth(i) {
                        tap(&mut keyboard, key, &mut expanded)?;
                    }
                    redraw = true;
                }
            }
            Event::Focus(new_class) => class = new_class,
            Event::ConfigChanged => match Config::load() {
                Ok(new_config) => config = new_config,
                Err(e) => tracing::error!("config not reloaded: {e:#}"),
            },
        }

        // Focus, the strip toggle, or a config save may have changed what to show.
        let wanted = config.layout_for(&class, expanded);
        if wanted != layout {
            layout = wanted;
            spans = keys::spans(&layout, width);
            pressed = None;
            redraw = true;
        }
        if redraw {
            draw(&mut display, &layout, pressed)?;
        }
    }
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

fn draw(display: &mut DrmDisplay, layout: &Layout, pressed: Option<usize>) -> anyhow::Result<()> {
    let (width, height) = display.size();
    let mut canvas = Canvas::new(width, height)?;
    keys::draw_row(&mut canvas, layout, pressed)?;
    display.present(&canvas)
}
