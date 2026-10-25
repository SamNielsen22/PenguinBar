use std::process::Command;

use anyhow::Context;

use crate::layout::Slider;

pub fn get(kind: Slider) -> anyhow::Result<f64> {
    match kind {
        Slider::Brightness => brightnessctl_level(&[]),
        Slider::Keyboard => brightnessctl_level(&["-d", &keyboard_device()?]),
        Slider::Volume => {
            let out = run("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"])?;
            parse_wpctl(&out).context("unexpected wpctl output")
        }
    }
}

pub fn set(kind: Slider, value: f64) {
    tracing::debug!("{kind:?} -> {}", percent(value, 0));
    std::thread::spawn(move || {
        let result = match kind {
            // 0% turns the screen off.
            Slider::Brightness => run("brightnessctl", &["set", &percent(value, 1)]),
            Slider::Keyboard => keyboard_device().and_then(|device| {
                run("brightnessctl", &["-d", &device, "set", &percent(value, 0)])
            }),
            Slider::Volume => run(
                "wpctl",
                &[
                    "set-volume",
                    "@DEFAULT_AUDIO_SINK@",
                    &format!("{:.2}", value.clamp(0.0, 1.0)),
                ],
            ),
        };
        if let Err(e) = result {
            tracing::warn!("slider: {e:#}");
        }
    });
}

fn percent(value: f64, min: u32) -> String {
    let percent = (value.clamp(0.0, 1.0) * 100.0).round() as u32;
    format!("{}%", percent.max(min))
}

fn run(program: &str, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !out.status.success() {
        anyhow::bail!("{program}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn brightnessctl_level(device: &[&str]) -> anyhow::Result<f64> {
    let out = run("brightnessctl", &[device, &["-m"]].concat())?;
    parse_brightnessctl(&out).context("unexpected brightnessctl output")
}

// Format: gmux_backlight,backlight,1023,100%,1023
fn parse_brightnessctl(line: &str) -> Option<f64> {
    let fields: Vec<&str> = line.trim().split(',').collect();
    let current: f64 = fields.get(2)?.parse().ok()?;
    let max: f64 = fields.get(4)?.parse().ok()?;
    if max > 0.0 {
        Some(current / max)
    } else {
        None
    }
}

// Format: Volume: 0.20 [MUTED]
fn parse_wpctl(line: &str) -> Option<f64> {
    line.split_whitespace().nth(1)?.parse().ok()
}

fn keyboard_device() -> anyhow::Result<String> {
    let out = run("brightnessctl", &["-l", "-m", "-c", "leds"])?;
    out.lines()
        .filter_map(|line| line.split(',').next())
        .find(|device| device.contains("kbd"))
        .map(String::from)
        .context("no keyboard backlight found")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_levels_from_the_tools_output() {
        assert_eq!(
            parse_brightnessctl("gmux_backlight,backlight,512,50%,1024\n"),
            Some(0.5)
        );
        assert_eq!(parse_brightnessctl("nonsense"), None);
        assert_eq!(parse_wpctl("Volume: 0.20\n"), Some(0.2));
        assert_eq!(parse_wpctl("Volume: 0.35 [MUTED]\n"), Some(0.35));
        assert_eq!(percent(0.0, 1), "1%");
        assert_eq!(percent(0.5, 0), "50%");
        assert_eq!(percent(2.0, 0), "100%");
    }
}
