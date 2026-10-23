use cairo::Context;

use super::{centred_text, colour, media, rounded_rect, Canvas};
use super::{BLACK, DIVIDER, DIVIDER_WIDTH, FONT, INSET, LABEL, PLATE, PLATE_PRESSED, TEXT_FONT};
use crate::layout::{Action, Item, Key, Layout};
use crate::media::NowPlaying;

// Sizes in pixels on the 2170 x 60 panel.
const MARGIN: f64 = 40.0;
const RIGHT_MARGIN: f64 = 16.0;
const KEY_WIDTH: f64 = 136.0;
const TOGGLE_WIDTH: f64 = 36.0;
// A workspace key is as wide as a 16:10 screen scaled to the bar.
const PREVIEW_WIDTH: f64 = 92.0;
const GROUP_GAP: f64 = 24.0;
const ICON_SIZE: f64 = 32.0;
const TOGGLE_SIZE: f64 = 48.0;
const TEXT_SIZE: f64 = 32.0;
const OUTLINE: f64 = 3.0;

// Each item's left and right edge in pixels, in layout order.
pub type Spans = Vec<(f64, f64)>;

pub fn spans(layout: &Layout, width: f64) -> Spans {
    // The strip hugs the right edge: lay it out from zero, then slide it over.
    let mut strip = Vec::new();
    let strip_width = place(&layout.strip, 0.0, &mut strip, 0.0);
    let shift = width - RIGHT_MARGIN - strip_width;

    let end = if layout.strip.is_empty() { width - RIGHT_MARGIN } else { shift - GROUP_GAP };
    let media_width = media_width(layout, end);
    let mut spans = Vec::new();
    let mut x = MARGIN;
    for group in &layout.groups {
        x = place(group, x, &mut spans, media_width) + GROUP_GAP;
    }
    spans.extend(strip.iter().map(|(l, r)| (l + shift, r + shift)));
    spans
}

// Lays one plate's items out from x. Returns where the plate ends.
fn place(items: &[Item], mut x: f64, spans: &mut Spans, media_width: f64) -> f64 {
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            x += DIVIDER_WIDTH;
        }
        let w = width_of(item, media_width);
        spans.push((x, x + w));
        x += w;
    }
    x
}

// The media widget stretches to fill whatever the keys leave free before `end`.
fn media_width(layout: &Layout, end: f64) -> f64 {
    let widgets = layout.items().filter(|item| **item == Item::Media).count();
    if widgets == 0 {
        return 0.0;
    }
    // Lay the keys out with the widget at zero width to see what is left over.
    let mut unused = Vec::new();
    let mut x = MARGIN;
    for group in &layout.groups {
        x = place(group, x, &mut unused, 0.0) + GROUP_GAP;
    }
    let free = end - (x - GROUP_GAP);
    (free / widgets as f64).max(media::MIN_WIDTH)
}

// The strip's chevron and the close button.
fn is_toggle(item: &Item) -> bool {
    matches!(item, Item::Key(key) if key.action == Action::ToggleStrip)
}

fn width_of(item: &Item, media_width: f64) -> f64 {
    match item {
        Item::Media => media_width,
        Item::Key(key) if matches!(key.action, Action::Workspace(_)) => PREVIEW_WIDTH,
        _ if is_toggle(item) => TOGGLE_WIDTH,
        _ => KEY_WIDTH,
    }
}

pub fn draw_row(
    canvas: &mut Canvas,
    layout: &Layout,
    pressed: Option<usize>,
    now: Option<&NowPlaying>,
) -> anyhow::Result<()> {
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
            // The close button is a bare glyph, and the media widget draws its own plate.
            let bare = plate.len() == 1 && (is_toggle(&plate[0]) || plate[0] == Item::Media);
            if !bare {
                colour(cr, PLATE);
                rounded_rect(cr, left, right, height);
                cr.fill()?;
            }

            for item in plate {
                let (l, r) = spans[i];
                if l > left {
                    colour(cr, DIVIDER);
                    cr.rectangle(l - DIVIDER_WIDTH, INSET, DIVIDER_WIDTH, height - 2.0 * INSET);
                    cr.fill()?;
                }
                match item {
                    Item::Key(key) => {
                        rounded_rect(cr, left, right, height);
                        cr.clip();
                        if pressed == Some(i) {
                            colour(cr, PLATE_PRESSED);
                            cr.rectangle(l, INSET, r - l, height - 2.0 * INSET);
                            cr.fill()?;
                        }
                        match &key.image {
                            Some(png) => image(cr, png, l, r, height)?,
                            // A workspace key is black until it has a picture.
                            None if matches!(key.action, Action::Workspace(_)) => {
                                colour(cr, BLACK);
                                cr.rectangle(l, INSET, r - l, height - 2.0 * INSET);
                                cr.fill()?;
                            }
                            None => label(cr, key, (l + r) / 2.0, height / 2.0)?,
                        }
                        // A lit key gets a white outline.
                        if key.lit {
                            colour(cr, LABEL);
                            cr.set_line_width(OUTLINE);
                            cr.rectangle(l + OUTLINE / 2.0, INSET + OUTLINE / 2.0, r - l - OUTLINE, height - 2.0 * INSET - OUTLINE);
                            cr.stroke()?;
                        }
                        cr.reset_clip();
                    }
                    Item::Media => {
                        if let Some(now) = now {
                            media::draw(cr, l, r - l, height, now)?;
                        }
                    }
                    // Replaced by keys before drawing.
                    Item::Workspaces => {}
                }
                i += 1;
            }
        }
        Ok(())
    })
}

pub fn key_at(spans: &Spans, x: f64) -> Option<usize> {
    spans.iter().position(|&(left, right)| x >= left && x < right)
}

// Draws a PNG scaled to fit inside the key, centred.
fn image(cr: &Context, png: &[u8], left: f64, right: f64, height: f64) -> anyhow::Result<()> {
    let surface = cairo::ImageSurface::create_from_png(&mut std::io::Cursor::new(png))?;
    let (w, h) = (surface.width() as f64, surface.height() as f64);
    let scale = ((right - left) / w).min((height - 2.0 * INSET) / h);
    let x = (left + right) / 2.0 - w * scale / 2.0;
    let y = height / 2.0 - h * scale / 2.0;
    cr.save()?;
    cr.translate(x, y);
    cr.scale(scale, scale);
    cr.set_source_surface(&surface, 0.0, 0.0)?;
    cr.paint()?;
    cr.restore()?;
    Ok(())
}

fn label(cr: &Context, key: &Key, x: f64, y: f64) -> anyhow::Result<()> {
    colour(cr, LABEL);
    if key.label.is_ascii() {
        return centred_text(cr, TEXT_FONT, TEXT_SIZE, &key.label, x, y);
    }
    let size = if key.action == Action::ToggleStrip { TOGGLE_SIZE } else { ICON_SIZE };
    centred_text(cr, FONT, size, &key.label, x, y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn key(label: &str) -> Item {
        Item::Key(Key { label: label.to_string(), action: Action::Command(String::new()), lit: false, image: None })
    }

    fn toggle() -> Item {
        Item::Key(Key { label: "<".to_string(), action: Action::ToggleStrip, lit: false, image: None })
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
    fn items_share_a_plate_with_dividers_and_groups_have_a_gap() {
        let layout = Layout { groups: vec![vec![key("a"), key("b")], vec![key("c")]], strip: vec![] };
        let spans = spans(&layout, 2170.0);
        assert_eq!(spans[1].0 - spans[0].1, DIVIDER_WIDTH);
        assert_eq!(spans[2].0 - spans[1].1, GROUP_GAP);
    }

    #[test]
    fn the_media_widget_fills_the_space_before_the_strip() {
        let layout = Layout {
            groups: vec![vec![key("esc")], vec![Item::Media], vec![key("b")]],
            strip: vec![toggle(), key("c")],
        };
        let with_strip = spans(&layout, 2170.0);
        let strip_left = with_strip[3].0;
        assert_eq!(with_strip[2].1, strip_left - GROUP_GAP);
        assert!(with_strip[1].1 - with_strip[1].0 > media::MIN_WIDTH);

        let no_strip = Layout { groups: vec![vec![Item::Media]], strip: vec![] };
        let alone = spans(&no_strip, 2170.0);
        assert_eq!(alone[0], (MARGIN, 2170.0 - RIGHT_MARGIN));
    }

    #[test]
    fn key_at_finds_items_and_ignores_the_gaps() {
        let layout = Layout { groups: vec![vec![key("a")], vec![key("b")]], strip: vec![] };
        let spans = spans(&layout, 2170.0);
        assert_eq!(key_at(&spans, MARGIN + 1.0), Some(0));
        assert_eq!(key_at(&spans, spans[1].0 + 1.0), Some(1));
        assert_eq!(key_at(&spans, spans[0].1 + 1.0), None);
        assert_eq!(key_at(&spans, 2000.0), None);
    }

    #[test]
    fn every_state_draws_without_error() {
        let mut lit = key("2");
        if let Item::Key(key) = &mut lit {
            key.lit = true;
            // A 1x1 PNG, to exercise the image path.
            let mut png = Vec::new();
            cairo::ImageSurface::create(cairo::Format::Rgb24, 1, 1).unwrap().write_to_png(&mut png).unwrap();
            key.image = Some(png);
        }
        let layout = Layout {
            groups: vec![vec![key("esc")], vec![toggle()], vec![key("1"), lit], vec![Item::Media]],
            strip: vec![toggle(), key("c")],
        };
        let now = NowPlaying {
            app: "firefox".to_string(),
            title: String::new(),
            playing: true,
            position: Duration::from_secs(64),
            length: Duration::from_secs(210),
        };
        let mut canvas = Canvas::new(2170, 60).unwrap();
        draw_row(&mut canvas, &layout, None, None).unwrap();
        draw_row(&mut canvas, &layout, Some(2), Some(&now)).unwrap();
        draw_row(&mut canvas, &Layout { groups: vec![], strip: vec![] }, None, None).unwrap();
    }
}
