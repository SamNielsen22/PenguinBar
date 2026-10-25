use std::fs::{File, OpenOptions};
use std::os::unix::io::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use drm::buffer::{Buffer, DrmFourcc};
use drm::control::dumbbuffer::DumbBuffer;
use drm::control::{connector, framebuffer, ClipRect, Device as ControlDevice, Mode};
use drm::Device;

use super::Display;
use crate::render::Canvas;

const BACKLIGHT: &str = "/sys/class/backlight/appletb_backlight/brightness";

// The drm crate's traits need a type we own.
pub struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl Device for Card {}
impl ControlDevice for Card {}

impl Card {
    fn open(path: &Path) -> anyhow::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        Ok(Card(file))
    }
}

// Card numbers change between boots, so match on the driver.
pub fn find_card() -> anyhow::Result<(PathBuf, Card)> {
    for entry in std::fs::read_dir("/dev/dri")? {
        let path = entry?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !name.starts_with("card") {
            continue;
        }
        let Ok(card) = Card::open(&path) else {
            continue;
        };
        let Ok(driver) = card.get_driver() else {
            continue;
        };
        if driver.name().to_string_lossy() == "appletbdrm" {
            return Ok((path, card));
        }
    }
    bail!("no appletbdrm card under /dev/dri. Is the bar in drm mode? Check `touchbar status`")
}

pub fn info() -> anyhow::Result<()> {
    let (path, card) = find_card()?;
    let driver = card.get_driver()?;
    println!("card:   {}", path.display());
    println!(
        "driver: {} ({})",
        driver.name().to_string_lossy(),
        driver.desc.to_string_lossy()
    );

    let res = card.resource_handles()?;
    for handle in res.connectors() {
        let conn = card.get_connector(*handle, false)?;
        println!(
            "connector {}-{}: {:?}",
            conn.interface().as_str(),
            conn.interface_id(),
            conn.state()
        );
        for mode in conn.modes() {
            let (w, h) = mode.size();
            println!("  mode {}x{} @ {} Hz", w, h, mode.vrefresh());
        }
    }
    Ok(())
}

pub fn set_backlight(level: u8) {
    // Driver ignores the first write.
    for _ in 0..2 {
        if let Err(e) = std::fs::write(BACKLIGHT, level.to_string()) {
            tracing::warn!("could not set Touch Bar backlight: {e}");
            return;
        }
    }
}

pub struct DrmDisplay {
    card: Card,
    mode: Mode,
    buffer: DumbBuffer,
    fb: framebuffer::Handle,
}

impl DrmDisplay {
    pub fn open() -> anyhow::Result<Self> {
        let (path, card) = find_card()?;
        tracing::info!("using {}", path.display());
        card.acquire_master_lock().context(
            "becoming DRM master. Is another program, like modetest, drawing on the bar?",
        )?;

        let res = card.resource_handles()?;
        let mut connected = None;
        for handle in res.connectors() {
            let conn = card.get_connector(*handle, false)?;
            if conn.state() == connector::State::Connected {
                connected = Some((*handle, conn));
                break;
            }
        }
        let (conn_handle, conn) =
            connected.context("no connected connector on the Touch Bar card")?;
        let mode = *conn.modes().first().context("connector reports no modes")?;
        let crtc = *res.crtcs().first().context("card has no CRTC")?;

        let (w, h) = mode.size();
        let buffer = card
            .create_dumb_buffer((w as u32, h as u32), DrmFourcc::Xrgb8888, 32)
            .context("creating dumb buffer")?;
        let fb = card
            .add_framebuffer(&buffer, 24, 32)
            .context("adding framebuffer")?;
        card.set_crtc(crtc, Some(fb), (0, 0), &[conn_handle], Some(mode))
            .context("setting mode")?;
        tracing::debug!(
            "mode {}x{} on {}-{}",
            w,
            h,
            conn.interface().as_str(),
            conn.interface_id()
        );

        set_backlight(255);

        Ok(Self {
            card,
            mode,
            buffer,
            fb,
        })
    }
}

impl Display for DrmDisplay {
    // The panel is 60x2170 portrait. We treat it as 2170x60.
    fn size(&self) -> (u32, u32) {
        let (w, h) = self.mode.size();
        (h as u32, w as u32)
    }

    fn present(&mut self, canvas: &Canvas) -> anyhow::Result<()> {
        let (w, h) = self.mode.size();
        let dst_stride = self.buffer.pitch() as usize;
        let row = w as usize * 4;

        let mut map = self
            .card
            .map_dumb_buffer(&mut self.buffer)
            .context("mapping dumb buffer")?;
        canvas.with_pixels(|src, src_stride| {
            for y in 0..h as usize {
                let dst = &mut map[y * dst_stride..y * dst_stride + row];
                dst.copy_from_slice(&src[y * src_stride..y * src_stride + row]);
            }
        })?;

        // Without this the driver never sends the frame over USB.
        let whole = ClipRect::new(0, 0, w, h);
        self.card
            .dirty_framebuffer(self.fb, &[whole])
            .context("flushing framebuffer")
    }
}

impl Drop for DrmDisplay {
    fn drop(&mut self) {
        let _ = self.card.destroy_framebuffer(self.fb);
        let _ = self.card.release_master_lock();
    }
}
