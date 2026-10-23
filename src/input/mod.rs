use std::sync::mpsc::Sender;

use anyhow::Context;
use evdev::{AbsoluteAxisCode, Device, EventSummary};

use crate::daemon::Event;

pub fn find_touch_device() -> anyhow::Result<Device> {
    for (path, device) in evdev::enumerate() {
        let name = device.name().unwrap_or("");
        if name.contains("iBridge") || name.contains("Touch Bar") {
            tracing::info!("touch: {name} at {}", path.display());
            return Ok(device);
        }
    }
    anyhow::bail!("no Touch Bar input device found")
}

// Reads the bar on a thread and sends a Touch event for every report.
// The bar reports about sixty times a second while a finger is on it, and
// goes quiet when the finger lifts. It never says "lifted" itself.
pub fn spawn_reader(mut device: Device, tx: Sender<Event>) -> anyhow::Result<()> {
    let max_x = device
        .get_absinfo()?
        .find(|(axis, _)| *axis == AbsoluteAxisCode::ABS_X)
        .map(|(_, info)| info.maximum() as f64)
        .context("touch device has no X axis")?;

    // Grab it so the compositor stops treating the bar as a mouse.
    device.grab().context("grabbing touch device")?;

    std::thread::spawn(move || {
        let mut x = 0.0;
        loop {
            let events = match device.fetch_events() {
                Ok(events) => events,
                Err(e) => {
                    tracing::error!("touch read failed: {e}");
                    return;
                }
            };
            for event in events {
                match event.destructure() {
                    EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_X, v) => x = v as f64 / max_x,
                    // A report is complete: say where the finger is.
                    EventSummary::Synchronization(..) => {
                        if tx.send(Event::Touch(x)).is_err() {
                            return;
                        }
                    }
                    _ => {}
                }
            }
        }
    });
    Ok(())
}
