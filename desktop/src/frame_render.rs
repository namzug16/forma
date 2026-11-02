//! Renders a frame in memory for display; nothing here writes to disk.

use std::sync::Arc;

use anyhow::Result;
use forma::diagnostics::Failure;
use forma::frames::{self, FrameFile};
use forma::inspect;
use forma::project::Project;
use forma::renderer;
use forma::scene::Scene;
use forma::svg_export;
use gpui_kit::RenderImage;
use image::{Frame, RgbaImage};
use serde_json::Value;
use smallvec::smallvec;
use tiny_skia::Pixmap;

/// Keeps frames sharp on Retina displays and when zooming to 200%.
const SHARP_SCALE_FACTOR: f64 = 2.0;

pub struct RenderedFrame {
    pub image: Arc<RenderImage>,
    /// Frame size in design units, independent of the render scale.
    pub width: f32,
    pub height: f32,
    pub report: Value,
}

/// Builds fresh resources so edits to tokens and assets show up.
pub fn render_frame(project: &Project, file: &FrameFile) -> Result<RenderedFrame> {
    let resources = project.resources()?;
    let scene = frames::load_scene(file, &resources)?;
    let scale = display_scale(&scene, project.config.render.scale);
    let canvas = renderer::render(&scene, scale).map_err(Failure::from)?;
    Ok(RenderedFrame {
        image: to_render_image(&canvas),
        width: scene.width as f32,
        height: scene.height as f32,
        report: inspect::report(&scene, None)?,
    })
}

/// Reads the file again, so the export is of what's on disk at the moment it was asked for.
pub fn export_svg(project: &Project, file: &FrameFile) -> Result<String> {
    let resources = project.resources()?;
    let scene = frames::load_scene(file, &resources)?;
    Ok(svg_export::to_svg(&scene))
}

/// Falls back to the project's own scale for frames too large to double.
fn display_scale(scene: &Scene, project_scale: f64) -> f64 {
    let sharp_scale = project_scale * SHARP_SCALE_FACTOR;
    if renderer::output_size(scene, sharp_scale).is_ok() {
        sharp_scale
    } else {
        project_scale
    }
}

pub fn to_render_image(canvas: &Pixmap) -> Arc<RenderImage> {
    bgra_render_image(canvas.width(), canvas.height(), to_straight_bgra(canvas))
}

/// `bgra` is straight, four bytes per pixel.
pub fn bgra_render_image(width: u32, height: u32, bgra: Vec<u8>) -> Arc<RenderImage> {
    let buffer =
        RgbaImage::from_raw(width, height, bgra).expect("the buffer holds four bytes per pixel");
    Arc::new(RenderImage::new(smallvec![Frame::new(buffer)]))
}

/// tiny-skia stores premultiplied RGBA; GPUI's image pipeline blends straight BGRA.
fn to_straight_bgra(canvas: &Pixmap) -> Vec<u8> {
    canvas
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.blue(), color.green(), color.red(), color.alpha()]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use tiny_skia::PremultipliedColorU8;

    use super::*;

    #[test]
    fn half_transparent_pixels_keep_their_color() {
        let mut canvas = Pixmap::new(1, 1).unwrap();
        canvas.pixels_mut()[0] = PremultipliedColorU8::from_rgba(100, 0, 50, 128).unwrap();

        assert_eq!(to_straight_bgra(&canvas), [100, 0, 199, 128]);
    }

    #[test]
    fn opaque_pixels_only_swap_channels() {
        let mut canvas = Pixmap::new(1, 1).unwrap();
        canvas.pixels_mut()[0] = PremultipliedColorU8::from_rgba(10, 20, 30, 255).unwrap();

        assert_eq!(to_straight_bgra(&canvas), [30, 20, 10, 255]);
    }
}
