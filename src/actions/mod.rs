use std::time::Duration;

use anyhow::Context;
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, KeyCode, KeyEvent};

pub struct Keyboard(VirtualDevice);

impl Keyboard {
    pub fn new() -> anyhow::Result<Self> {
        // 0x2ff is KEY_MAX. Register every key so any config works.
        let keys: AttributeSet<KeyCode> = (0..=0x2ff).map(KeyCode).collect();
        let device = VirtualDevice::builder()?
            .name("penguinbar")
            .with_keys(&keys)?
            .build()
            .context("creating virtual keyboard")?;
        // Compositor needs a moment to pick up the new device.
        std::thread::sleep(Duration::from_millis(200));
        Ok(Keyboard(device))
    }

    // Press the keys in order, release in reverse.
    pub fn tap(&mut self, keys: &[KeyCode]) -> anyhow::Result<()> {
        for key in keys {
            self.0.emit(&[*KeyEvent::new(*key, 1)])?;
        }
        for key in keys.iter().rev() {
            self.0.emit(&[*KeyEvent::new(*key, 0)])?;
        }
        Ok(())
    }
}

// Key names joined with +, like KEY_LEFTCTRL+KEY_LEFTSHIFT+KEY_P.
pub fn parse_keys(names: &str) -> anyhow::Result<Vec<KeyCode>> {
    let mut keys = Vec::new();
    for name in names.split('+') {
        let key = name
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("unknown key {name}. Names look like KEY_ESC"))?;
        keys.push(key);
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_single_keys_and_combinations() {
        assert_eq!(parse_keys("KEY_ESC").unwrap(), vec![KeyCode::KEY_ESC]);
        assert_eq!(
            parse_keys("KEY_LEFTCTRL + KEY_C").unwrap(),
            vec![KeyCode::KEY_LEFTCTRL, KeyCode::KEY_C]
        );
        assert!(parse_keys("KEY_LEFTCTRL+KEY_NOPE").is_err());
        assert!(parse_keys("").is_err());
    }
}
