pub mod hyprland;

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::Once;

use anyhow::Context;

use crate::daemon::Event;

// The focused window's class, like "firefox", and title.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Window {
    pub class: String,
    pub title: String,
}

// A window on a workspace: its id for the screenshot tool, and where it
// sits on the monitor in logical pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub window: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

// The workspaces, the active one and its monitor, the windows on each
// workspace, and the latest small picture of each as PNG.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Workspaces {
    pub ids: Vec<i32>,
    pub active: i32,
    pub monitor: String,
    // The monitor's size in logical pixels.
    pub screen: (f64, f64),
    pub windows: HashMap<i32, Vec<Placed>>,
    pub previews: HashMap<i32, Vec<u8>>,
}

// What to photograph for a workspace: the screen while it is showing,
// or its windows laid out to scale while it is hidden.
#[derive(Clone, Debug)]
pub enum Shot {
    Screen(String),
    Windows(Vec<Placed>, (f64, f64)),
}

fn on_hyprland() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}

// Starts a thread that sends Focus and Workspaces events as the desktop changes.
// One function per desktop; the first one that matches wins.
pub fn spawn_watcher(tx: Sender<Event>) -> anyhow::Result<()> {
    if on_hyprland() {
        return hyprland::spawn_watcher(tx);
    }
    tracing::warn!("no supported desktop found, so the bar will never show app keys");
    Ok(())
}

pub fn switch_workspace(id: i32) -> anyhow::Result<()> {
    if on_hyprland() {
        return hyprland::switch_workspace(id);
    }
    anyhow::bail!("no supported desktop to switch workspaces on")
}

// Previews are this wide; the height follows the window's shape.
const PREVIEW_WIDTH: usize = 92;

impl Workspaces {
    // A shot of every workspace, the active one first.
    pub fn all_shots(&self) -> Vec<(i32, Shot)> {
        let mut shots = self.active_shot();
        for id in &self.ids {
            if let Some(windows) = self.windows.get(id).filter(|_| *id != self.active) {
                shots.push((*id, Shot::Windows(windows.clone(), self.screen)));
            }
        }
        shots
    }

    // A shot of the active workspace only.
    pub fn active_shot(&self) -> Vec<(i32, Shot)> {
        vec![(self.active, Shot::Screen(self.monitor.clone()))]
    }
}

// Takes each listed shot on a thread and sends them as Preview events.
pub fn spawn_previews(shots: Vec<(i32, Shot)>, tx: Sender<Event>) {
    std::thread::spawn(move || {
        for (workspace, shot) in shots {
            match capture(&shot) {
                Ok(png) => {
                    if tx.send(Event::Preview(workspace, png)).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    static ONCE: Once = Once::new();
                    ONCE.call_once(|| tracing::warn!("workspace previews: {e:#}"));
                }
            }
        }
    });
}

fn capture(shot: &Shot) -> anyhow::Result<Vec<u8>> {
    let picture = match shot {
        Shot::Screen(monitor) => {
            let screen = grab(&["-o", monitor])?;
            let scale = PREVIEW_WIDTH as f64 / screen.width as f64;
            screen.shrink(PREVIEW_WIDTH, (screen.height as f64 * scale).round() as usize)
        }
        // Each window shrunk and placed where it sits on screen, on black.
        Shot::Windows(windows, (screen_w, screen_h)) => {
            let scale = PREVIEW_WIDTH as f64 / screen_w;
            let mut canvas = Image::black(PREVIEW_WIDTH, (screen_h * scale).round() as usize);
            for placed in windows {
                let window = grab(&["-T", &placed.window])?;
                let small = window.shrink((placed.width * scale).round() as usize, (placed.height * scale).round() as usize);
                canvas.blit(&small, (placed.x * scale).round() as usize, (placed.y * scale).round() as usize);
            }
            canvas
        }
    };
    picture.png()
}

// Runs grim at full size. Scaling is done in `Image::shrink`.
fn grab(args: &[&str]) -> anyhow::Result<Image> {
    let out = std::process::Command::new("grim").args(args).args(["-t", "ppm", "-"]).output()?;
    if !out.status.success() {
        anyhow::bail!("grim: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Image::from_ppm(&out.stdout).context("grim gave an unexpected image")
}

// A plain RGB picture.
struct Image {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
}

impl Image {
    fn black(width: usize, height: usize) -> Image {
        Image { width, height, rgb: vec![0; width * height * 3] }
    }

    // A PPM is "P6", width, height, 255, then RGB bytes.
    fn from_ppm(bytes: &[u8]) -> Option<Image> {
        let header_end = bytes.iter().enumerate().filter(|(_, b)| b.is_ascii_whitespace()).nth(3)?.0;
        let header = std::str::from_utf8(&bytes[..header_end]).ok()?;
        let mut parts = header.split_whitespace();
        if parts.next()? != "P6" {
            return None;
        }
        let width: usize = parts.next()?.parse().ok()?;
        let height: usize = parts.next()?.parse().ok()?;
        let rgb = bytes.get(header_end + 1..header_end + 1 + width * height * 3)?.to_vec();
        Some(Image { width, height, rgb })
    }

    // A smaller copy. Each new pixel averages a few samples spread over the
    // block it covers.
    fn shrink(&self, width: usize, height: usize) -> Image {
        let (width, height) = (width.max(1), height.max(1));
        let mut rgb = Vec::with_capacity(width * height * 3);
        for ty in 0..height {
            let ys = samples(block(ty, height, self.height));
            for tx in 0..width {
                let xs = samples(block(tx, width, self.width));
                let mut sum = [0usize; 3];
                for y in &ys {
                    for x in &xs {
                        let i = (y * self.width + x) * 3;
                        sum[0] += self.rgb[i] as usize;
                        sum[1] += self.rgb[i + 1] as usize;
                        sum[2] += self.rgb[i + 2] as usize;
                    }
                }
                let n = ys.len() * xs.len();
                rgb.extend([(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8]);
            }
        }
        Image { width, height, rgb }
    }

    // Copies another picture onto this one at (x, y), clipped to the edges.
    fn blit(&mut self, other: &Image, x: usize, y: usize) {
        for row in 0..other.height {
            if y + row >= self.height {
                break;
            }
            let count = other.width.min(self.width.saturating_sub(x));
            let from = row * other.width * 3;
            let to = ((y + row) * self.width + x) * 3;
            self.rgb[to..to + count * 3].copy_from_slice(&other.rgb[from..from + count * 3]);
        }
    }

    fn png(&self) -> anyhow::Result<Vec<u8>> {
        let mut surface = cairo::ImageSurface::create(cairo::Format::Rgb24, self.width as i32, self.height as i32)?;
        let stride = surface.stride() as usize;
        {
            let mut data = surface.data()?;
            for y in 0..self.height {
                for x in 0..self.width {
                    let i = (y * self.width + x) * 3;
                    let o = y * stride + x * 4;
                    data[o..o + 4].copy_from_slice(&[self.rgb[i + 2], self.rgb[i + 1], self.rgb[i], 0]);
                }
            }
        }
        let mut png = Vec::new();
        surface.write_to_png(&mut png)?;
        Ok(png)
    }
}

// The range of source pixels that target pixel `i` of `count` covers,
// at least one pixel wide.
fn block(i: usize, count: usize, source: usize) -> (usize, usize) {
    let start = i * source / count;
    let end = ((i + 1) * source / count).max(start + 1).min(source);
    (start.min(end - 1), end)
}

// Up to four positions spread evenly through a range.
fn samples((start, end): (usize, usize)) -> Vec<usize> {
    let len = end - start;
    let n = len.min(4);
    (0..n).map(|k| start + (2 * k + 1) * len / (2 * n)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_ppm_shrinks_by_averaging_and_blits() {
        let mut ppm = b"P6\n2 2\n255\n".to_vec();
        ppm.extend_from_slice(&[255, 0, 0, 255, 0, 0, 0, 0, 255, 0, 0, 255]);
        let image = Image::from_ppm(&ppm).unwrap();
        assert_eq!((image.width, image.height), (2, 2));
        let small = image.shrink(1, 1);
        assert_eq!(small.rgb, vec![127, 0, 127]);
        assert_eq!(samples((0, 2)), vec![0, 1]);
        assert_eq!(samples((0, 40)), vec![5, 15, 25, 35]);
        assert!(Image::from_ppm(b"P5\n1 1\n255\n\0").is_none());

        let mut canvas = Image::black(3, 1);
        canvas.blit(&small, 2, 0);
        assert_eq!(canvas.rgb, vec![0, 0, 0, 0, 0, 0, 127, 0, 127]);
        canvas.blit(&image, 2, 0);
        assert_eq!(&canvas.rgb[6..], &[255, 0, 0]);
    }
}
