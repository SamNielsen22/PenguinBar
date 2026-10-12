use std::f64::consts::{FRAC_PI_2, PI};

use cairo::{Context, FontSlant, FontWeight};

use super::{Canvas, Rgb, FONT};
use crate::layout::{Action, Key, Layout};

// Sizes measured from Apple's Touch Bar, in pixels on the 2170 x 60 panel.
const MARGIN: f64 = 40.0;
const RIGHT_MARGIN: f64 = 16.0;
const KEY_WIDTH: f64 = 136.0;
const TOGGLE_WIDTH: f64 = 36.0;
const DIVIDER: f64 = 2.0;
const GROUP_GAP: f64 = 24.0;
const INSET: f64 = 2.0;
const RADIUS: f64 = 8.0;
const ICON_SIZE: f64 = 32.0;
const TOGGLE_SIZE: f64 = 48.0;
const TEXT_SIZE: f64 = 32.0;
const TEXT_FONT: &str = "sans-serif";
const BLACK: Rgb = (0.0, 0.0, 0.0);
const PLATE: Rgb = (0.22, 0.22, 0.22);
const PLATE_PRESSED: Rgb = (0.45, 0.45, 0.45);
const DIVIDER_COLOUR: Rgb = (0.14, 0.14, 0.14);
const LABEL: Rgb = (1.0, 1.0, 1.0);

// Each key's left and right edge in pixels, in layout order.
pub type Spans = Vec<(f64, f64)>;

pub fn spans(layout: &Layout, width: f64) -> Spans {
    let mut spans = Vec::new();
    let mut x = MARGIN;
    for group in &layout.groups {
        x = place(group, x, &mut spans) + GROUP_GAP;
    }
    // The strip hugs the right edge: lay it out from zero, then slide it over.
    let mut strip = Vec::new();
    let strip_width = place(&layout.strip, 0.0, &mut strip);
    let shift = width - RIGHT_MARGIN - strip_width;
    spans.extend(strip.iter().map(|(l, r)| (l + shift, r + shift)));
    spans
}

// Lays one plate's keys out from x. Returns where the plate ends.
fn place(keys: &[Key], mut x: f64, spans: &mut Spans) -> f64 {
    for (i, key) in keys.iter().enumerate() {
        if i > 0 {
            x += DIVIDER;
        }
        spans.push((x, x + width_of(key)));
        x += width_of(key);
    }
    x
}

fn width_of(key: &Key) -> f64 {
    if key.action == Action::ToggleStrip { TOGGLE_WIDTH } else { KEY_WIDTH }
}

pub fn draw_row(canvas: &mut Canvas, layout: &Layout, pressed: Option<usize>) -> anyhow::Result<()> {
    let (width, height) = canvas.size();
    let height = height as f64;
    let spans = spans(layout, width as f64);

    canvas.fill(BLACK)?;
    canvas.draw(|cr| {
        let mut i = 0;
        for plate in layout.plates() {
            if plate.is_empty() {
                continue;
            }
            let left = spans[i].0;
            let right = spans[i + plate.len() - 1].1;
            // The close button stands alone and is a bare glyph, like Apple's.
            let bare = plate.len() == 1 && plate[0].action == Action::ToggleStrip;
            if !bare {
                set_colour(cr, PLATE);
                rounded_rect(cr, left, right, height);
                cr.fill()?;
            }

            for key in plate {
                let (l, r) = spans[i];
                if pressed == Some(i) {
                    rounded_rect(cr, left, right, height);
                    cr.clip();
                    set_colour(cr, PLATE_PRESSED);
                    cr.rectangle(l, INSET, r - l, height - 2.0 * INSET);
                    cr.fill()?;
                    cr.reset_clip();
                }
                if l > left {
                    set_colour(cr, DIVIDER_COLOUR);
                    cr.rectangle(l - DIVIDER, INSET, DIVIDER, height - 2.0 * INSET);
                    cr.fill()?;
                }
                label(cr, key, (l + r) / 2.0, height / 2.0)?;
                i += 1;
            }
        }
        Ok(())
    })
}

pub fn key_at(spans: &Spans, x: f64) -> Option<usize> {
    spans.iter().position(|&(left, right)| x >= left && x < right)
}

// Draws the key's label centred on (x, y).
fn label(cr: &Context, key: &Key, x: f64, y: f64) -> anyhow::Result<()> {
    if key.label.is_ascii() {
        cr.select_font_face(TEXT_FONT, FontSlant::Normal, FontWeight::Normal);
        cr.set_font_size(TEXT_SIZE);
    } else {
        cr.select_font_face(FONT, FontSlant::Normal, FontWeight::Normal);
        let size = if key.action == Action::ToggleStrip { TOGGLE_SIZE } else { ICON_SIZE };
        cr.set_font_size(size);
    }
    set_colour(cr, LABEL);
    let ext = cr.text_extents(&key.label)?;
    cr.move_to(x - ext.width() / 2.0 - ext.x_bearing(), y - ext.height() / 2.0 - ext.y_bearing());
    cr.show_text(&key.label)?;
    Ok(())
}

fn set_colour(cr: &Context, (r, g, b): Rgb) {
    cr.set_source_rgb(r, g, b);
}

// A plate from left to right, nearly the full height of the bar.
fn rounded_rect(cr: &Context, left: f64, right: f64, height: f64) {
    let (top, bottom) = (INSET, height - INSET);
    cr.new_sub_path();
    cr.arc(right - RADIUS, top + RADIUS, RADIUS, -FRAC_PI_2, 0.0);
    cr.arc(right - RADIUS, bottom - RADIUS, RADIUS, 0.0, FRAC_PI_2);
    cr.arc(left + RADIUS, bottom - RADIUS, RADIUS, FRAC_PI_2, PI);
    cr.arc(left + RADIUS, top + RADIUS, RADIUS, PI, 3.0 * FRAC_PI_2);
    cr.close_path();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(label: &str) -> Key {
        Key { label: label.to_string(), action: Action::Command(String::new()) }
    }

    fn toggle() -> Key {
        Key { label: "<".to_string(), action: Action::ToggleStrip }
    }

    #[test]
    fn groups_start_at_the_margin_and_the_strip_ends_at_the_right() {
        let layout = Layout { groups: vec![vec![key("esc")]], strip: vec![toggle(), key("a")] };
        let spans = spans(&layout, 2170.0);
        assert_eq!(spans[0], (MARGIN, MARGIN + KEY_WIDTH));
        assert_eq!(spans[1].1 - spans[1].0, TOGGLE_WIDTH);
        assert_eq!(spans[2].1, 2170.0 - RIGHT_MARGIN);
    }

    #[test]
    fn keys_share_a_plate_with_dividers_and_groups_have_a_gap() {
        let layout = Layout { groups: vec![vec![key("a"), key("b")], vec![key("c")]], strip: vec![] };
        let spans = spans(&layout, 2170.0);
        assert_eq!(spans[1].0 - spans[0].1, DIVIDER);
        assert_eq!(spans[2].0 - spans[1].1, GROUP_GAP);
    }

    #[test]
    fn key_at_finds_keys_and_ignores_the_gaps() {
        let layout = Layout { groups: vec![vec![key("a")], vec![key("b")]], strip: vec![] };
        let spans = spans(&layout, 2170.0);
        assert_eq!(key_at(&spans, MARGIN + 1.0), Some(0));
        assert_eq!(key_at(&spans, spans[1].0 + 1.0), Some(1));
        assert_eq!(key_at(&spans, spans[0].1 + 1.0), None);
        assert_eq!(key_at(&spans, 2000.0), None);
    }

    #[test]
    fn every_state_draws_without_error() {
        let layout = Layout {
            groups: vec![vec![key("esc")], vec![toggle()], vec![key("\u{f05a8}"), key("b")]],
            strip: vec![toggle(), key("c")],
        };
        let mut canvas = Canvas::new(2170, 60).unwrap();
        draw_row(&mut canvas, &layout, None).unwrap();
        draw_row(&mut canvas, &layout, Some(2)).unwrap();
        draw_row(&mut canvas, &Layout { groups: vec![], strip: vec![] }, None).unwrap();
    }
}
