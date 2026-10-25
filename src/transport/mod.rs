pub mod drm;

use crate::render::Canvas;

pub trait Display {
    fn size(&self) -> (u32, u32);
    fn present(&mut self, canvas: &Canvas) -> anyhow::Result<()>;
}
