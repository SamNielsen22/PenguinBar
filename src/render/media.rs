use std::time::Duration;

use cairo::Context;

use super::{
    centred_text, colour, rounded_rect, ACCENT, DIVIDER, DIVIDER_WIDTH, FONT, LABEL, PLATE,
    TEXT_FONT, TRACK,
};
use crate::media::NowPlaying;

pub const MIN_WIDTH: f64 = 500.0;
const BUTTON: f64 = 136.0;
const TIME: f64 = 110.0;
const BAR_HEIGHT: f64 = 4.0;
const PLAYHEAD_HEIGHT: f64 = 32.0;
const ICON_SIZE: f64 = 32.0;
const TEXT_SIZE: f64 = 24.0;

pub enum Hit {
    Button,
    Bar(f64),
}

// x is from the widget's left edge.
pub fn hit(x: f64, width: f64) -> Hit {
    if x < BUTTON {
        return Hit::Button;
    }
    let (start, end) = bar(width);
    Hit::Bar(((x - start) / (end - start)).clamp(0.0, 1.0))
}

fn bar(width: f64) -> (f64, f64) {
    (BUTTON + TIME, width - TIME)
}

pub fn draw(
    cr: &Context,
    left: f64,
    width: f64,
    height: f64,
    now: &NowPlaying,
) -> anyhow::Result<()> {
    let mid = height / 2.0;

    colour(cr, PLATE);
    rounded_rect(cr, left, left + width, height);
    cr.fill()?;
    colour(cr, DIVIDER);
    cr.rectangle(left + BUTTON, 0.0, DIVIDER_WIDTH, height);
    cr.fill()?;

    let icon = if now.playing {
        "\u{f03e4}"
    } else {
        "\u{f040a}"
    };
    colour(cr, LABEL);
    centred_text(cr, FONT, ICON_SIZE, icon, left + BUTTON / 2.0, mid)?;

    let left_over = now.length.saturating_sub(now.position);
    centred_text(
        cr,
        TEXT_FONT,
        TEXT_SIZE,
        &clock(now.position),
        left + BUTTON + TIME / 2.0,
        mid,
    )?;
    centred_text(
        cr,
        TEXT_FONT,
        TEXT_SIZE,
        &format!("-{}", clock(left_over)),
        left + width - TIME / 2.0,
        mid,
    )?;

    let (start, end) = bar(width);
    let (start, end) = (left + start, left + end);
    let fraction = if now.length.is_zero() {
        0.0
    } else {
        now.position.as_secs_f64() / now.length.as_secs_f64()
    };
    let playhead = start + (end - start) * fraction.clamp(0.0, 1.0);
    colour(cr, TRACK);
    cr.rectangle(start, mid - BAR_HEIGHT / 2.0, end - start, BAR_HEIGHT);
    cr.fill()?;
    colour(cr, ACCENT);
    cr.rectangle(
        playhead - 1.5,
        mid - PLAYHEAD_HEIGHT / 2.0,
        3.0,
        PLAYHEAD_HEIGHT,
    );
    cr.fill()?;
    Ok(())
}

fn clock(d: Duration) -> String {
    let secs = d.as_secs();
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_button_is_at_the_left_and_the_bar_runs_between_the_times() {
        let width = 800.0;
        assert!(matches!(hit(10.0, width), Hit::Button));
        let (start, end) = bar(width);
        assert!(matches!(hit(start, width), Hit::Bar(f) if f == 0.0));
        assert!(matches!(hit((start + end) / 2.0, width), Hit::Bar(f) if (f - 0.5).abs() < 0.01));
        assert!(matches!(hit(width, width), Hit::Bar(f) if f == 1.0));
    }

    #[test]
    fn times_read_like_a_clock() {
        assert_eq!(clock(Duration::from_secs(7)), "00:07");
        assert_eq!(clock(Duration::from_secs(187)), "03:07");
        assert_eq!(clock(Duration::from_secs(3600)), "60:00");
    }
}
