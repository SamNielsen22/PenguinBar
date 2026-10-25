use std::f64::consts::TAU;

use cairo::Context;

use super::{centred_text, colour, ACCENT, FONT, LABEL, TRACK};
use crate::layout::Slider;

pub const WIDTH: f64 = 670.0;
const ICON: f64 = 64.0;
const ICON_SIZE: f64 = 28.0;
const TRACK_HEIGHT: f64 = 4.0;
const KNOB: f64 = 11.0;

// x is from the slider's left edge.
pub fn hit(x: f64) -> f64 {
    let (start, end) = track();
    ((x - start) / (end - start)).clamp(0.0, 1.0)
}

pub fn knob_at(value: f64) -> f64 {
    let (start, end) = track();
    start + (end - start) * value.clamp(0.0, 1.0)
}

fn track() -> (f64, f64) {
    (ICON + KNOB, WIDTH - ICON - KNOB)
}

fn icons(kind: Slider) -> (&'static str, &'static str) {
    match kind {
        Slider::Brightness => ("\u{f0599}", "\u{f05a8}"),
        Slider::Volume => ("\u{f057f}", "\u{f057e}"),
        Slider::Keyboard => ("\u{f030c}", "\u{f030c}"),
    }
}

pub fn draw(cr: &Context, left: f64, height: f64, kind: Slider, value: f64) -> anyhow::Result<()> {
    let mid = height / 2.0;
    let (low, high) = icons(kind);
    colour(cr, LABEL);
    centred_text(cr, FONT, ICON_SIZE, low, left + ICON / 2.0, mid)?;
    centred_text(cr, FONT, ICON_SIZE, high, left + WIDTH - ICON / 2.0, mid)?;

    let (start, end) = track();
    let (start, end) = (left + start, left + end);
    let knob = left + knob_at(value);
    colour(cr, TRACK);
    cr.rectangle(start, mid - TRACK_HEIGHT / 2.0, end - start, TRACK_HEIGHT);
    cr.fill()?;
    colour(cr, ACCENT);
    cr.rectangle(start, mid - TRACK_HEIGHT / 2.0, knob - start, TRACK_HEIGHT);
    cr.fill()?;
    cr.arc(knob, mid, KNOB, 0.0, TAU);
    cr.fill()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_track_runs_between_the_icons() {
        let (start, end) = track();
        assert_eq!(hit(0.0), 0.0);
        assert_eq!(hit(start), 0.0);
        assert!((hit((start + end) / 2.0) - 0.5).abs() < 0.001);
        assert_eq!(hit(WIDTH), 1.0);
        assert_eq!(hit(knob_at(0.3)), 0.3);
    }
}
