use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::mpsc::Sender;

use anyhow::Context;
use evdev::Device;

use crate::daemon::Event;

// The bar reports touches as 52-byte HID reports: ten fingers of four bytes
// each, then twelve bytes that are not used here. A finger is a flags byte
// with the touch bit at bit 4, a 16-bit X and an 8-bit Y.
const REPORT_SIZE: usize = 52;
const FINGERS: usize = 10;
const TOUCHING: u8 = 0x10;
const MAX_X: f64 = 32767.0;

// Apple's vendor id and the iBridge product id, with the touchpad descriptor.
const IBRIDGE: &str = "HID_ID=0003:000005AC:00008600";
const TOUCHPAD_DESCRIPTOR: [u8; 4] = [0x05, 0x0d, 0x09, 0x05];

pub fn find_touch_device() -> anyhow::Result<File> {
    for entry in std::fs::read_dir("/sys/class/hidraw")? {
        let entry = entry?;
        let uevent = std::fs::read_to_string(entry.path().join("device/uevent")).unwrap_or_default();
        let descriptor = std::fs::read(entry.path().join("device/report_descriptor")).unwrap_or_default();
        if uevent.contains(IBRIDGE) && descriptor.starts_with(&TOUCHPAD_DESCRIPTOR) {
            let path = Path::new("/dev").join(entry.file_name());
            tracing::info!("touch: {}", path.display());
            return File::open(&path)
                .with_context(|| format!("opening {}. Can the input group read it?", path.display()));
        }
    }
    anyhow::bail!("no Touch Bar found among the HID devices")
}

// The compositor also sees the bar as a mouse. Grabbing that device stops it.
fn grab_pointer() -> anyhow::Result<Option<Device>> {
    for (_, mut device) in evdev::enumerate() {
        if device.name().unwrap_or("").contains("iBridge Touchpad") {
            device.grab().context("grabbing the bar's pointer device")?;
            return Ok(Some(device));
        }
    }
    Ok(None)
}

// Where the first touching finger is, 0.0 to 1.0, or None when no finger touches.
fn finger_x(report: &[u8]) -> Option<f64> {
    report
        .chunks_exact(4)
        .take(FINGERS)
        .find(|finger| finger[0] & TOUCHING != 0)
        .map(|finger| u16::from_le_bytes([finger[1], finger[2]]) as f64 / MAX_X)
}

// Reads the bar on a thread and sends a Touch for every report with a finger
// down and a Release for the report that says it lifted.
pub fn spawn_reader(mut device: File, tx: Sender<Event>) -> anyhow::Result<()> {
    let pointer = grab_pointer()?;
    std::thread::spawn(move || {
        let _pointer = pointer;
        let mut report = [0u8; REPORT_SIZE];
        loop {
            if let Err(e) = device.read_exact(&mut report) {
                tracing::error!("touch read failed: {e}");
                return;
            }
            let event = match finger_x(&report) {
                Some(x) => Event::Touch(x),
                None => Event::Release,
            };
            if tx.send(event).is_err() {
                return;
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_first_touching_finger() {
        let mut report = [0u8; REPORT_SIZE];
        // Finger 0 lifted at x = 100, finger 1 touching at x = 16383.
        report[0..4].copy_from_slice(&[0x01, 100, 0, 63]);
        report[4..8].copy_from_slice(&[0x32, 0xff, 0x3f, 63]);
        let x = finger_x(&report).unwrap();
        assert!((x - 0.5).abs() < 0.001);
        report[4] = 0x02;
        assert_eq!(finger_x(&report), None);
    }
}
