pub mod keys;
pub mod media;
pub mod slider;

use std::f64::consts::{FRAC_PI_2, PI};
use std::fs::File;
use std::path::Path;

use anyhow::Context as _;
use cairo::{Context, FontSlant, FontWeight, Format, ImageSurface};

pub const FONT: &str = "JetBrainsMono Nerd Font";
pub const TEXT_FONT: &str = "sans-serif";

pub type Rgb = (f64, f64, f64);

// Keys use the desktop theme colours. The background stays black.
pub const BACKGROUND: Rgb = rgb(0x000000);
pub const PLATE: Rgb = rgb(0x2a2b22);
pub const DIVIDER: Rgb = rgb(0x1b1c15);
pub const TRACK: Rgb = rgb(0x46483b);
pub const LABEL: Rgb = rgb(0xe4e3d6);
pub const ACCENT: Rgb = rgb(0xc1ce7d);
pub const ON_ACCENT: Rgb = rgb(0x2c3400);

const fn rgb(hex: u32) -> Rgb {
    (
        ((hex >> 16) & 0xff) as f64 / 255.0,
        ((hex >> 8) & 0xff) as f64 / 255.0,
        (hex & 0xff) as f64 / 255.0,
    )
}

pub const DIVIDER_WIDTH: f64 = 4.0;
const RADIUS: f64 = 6.0;

pub fn colour(cr: &Context, (r, g, b): Rgb) {
    cr.set_source_rgb(r, g, b);
}

pub fn rounded_rect(cr: &Context, left: f64, right: f64, height: f64) {
    let r = RADIUS;
    cr.new_sub_path();
    cr.arc(right - r, r, r, -FRAC_PI_2, 0.0);
    cr.arc(right - r, height - r, r, 0.0, FRAC_PI_2);
    cr.arc(left + r, height - r, r, FRAC_PI_2, PI);
    cr.arc(left + r, r, r, PI, 3.0 * FRAC_PI_2);
    cr.close_path();
}

pub fn centred_text(
    cr: &Context,
    font: &str,
    size: f64,
    text: &str,
    x: f64,
    y: f64,
) -> anyhow::Result<()> {
    cr.select_font_face(font, FontSlant::Normal, FontWeight::Normal);
    cr.set_font_size(size);
    let ext = cr.text_extents(text)?;
    cr.move_to(
        x - ext.width() / 2.0 - ext.x_bearing(),
        y - ext.height() / 2.0 - ext.y_bearing(),
    );
    cr.show_text(text)?;
    Ok(())
}

pub struct Canvas {
    surface: ImageSurface,
    width: u32,
    height: u32,
}

impl Canvas {
    // The panel is mounted sideways, so the surface is 60x2170 and everything is rotated.
    pub fn new(width: u32, height: u32) -> anyhow::Result<Self> {
        let surface = ImageSurface::create(Format::Rgb24, height as i32, width as i32)
            .context("creating cairo surface")?;
        Ok(Self {
            surface,
            width,
            height,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn draw<F>(&mut self, f: F) -> anyhow::Result<()>
    where
        F: FnOnce(&Context) -> anyhow::Result<()>,
    {
        let cr = Context::new(&self.surface).context("creating cairo context")?;
        cr.translate(self.height as f64, 0.0);
        cr.rotate(FRAC_PI_2);
        f(&cr)
    }

    pub fn fill(&mut self, (r, g, b): Rgb) -> anyhow::Result<()> {
        self.draw(|cr| {
            cr.set_source_rgb(r, g, b);
            cr.paint()?;
            Ok(())
        })
    }

    pub fn with_pixels<F>(&self, f: F) -> anyhow::Result<()>
    where
        F: FnOnce(&[u8], usize),
    {
        let stride = self.surface.stride() as usize;
        self.surface
            .with_data(|data| f(data, stride))
            .map_err(|e| anyhow::anyhow!("reading canvas pixels: {e}"))
    }

    pub fn save_png(&self, path: &Path) -> anyhow::Result<()> {
        let out = ImageSurface::create(Format::Rgb24, self.width as i32, self.height as i32)?;
        let cr = Context::new(&out)?;
        // Rotate back to make the PNG landscape.
        cr.translate(0.0, self.height as f64);
        cr.rotate(-FRAC_PI_2);
        cr.set_source_surface(&self.surface, 0.0, 0.0)?;
        cr.paint()?;
        drop(cr);

        let mut file =
            File::create(path).with_context(|| format!("creating {}", path.display()))?;
        out.write_to_png(&mut file)?;
        Ok(())
    }
}
