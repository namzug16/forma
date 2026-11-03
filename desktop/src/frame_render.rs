//! Renders a frame in memory for display; nothing here writes to disk.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use forma::assets::ImageCache;
use forma::diagnostics::Failure;
use forma::document;
use forma::frames::{self, FrameFile};
use forma::inspect;
use forma::project::Project;
use forma::renderer;
use forma::svg_export;
use forma::tokens::Tokens;
use gpui_kit::{RenderImage, Size, size};
use image::{Frame, RgbaImage};
use serde_json::Value;
use smallvec::smallvec;
use tiny_skia::Pixmap;

use crate::layer_details::{self, AuthoredProperty};
use crate::viewport::Viewport;

/// About what a fixed 2× render of a tall phone screenshot used to be, so zooming in never costs
/// more than that did.
// shortcut: a frame zoomed in past this is magnified and goes soft, render only the visible
// region if close inspection matters
const MAX_PREVIEW_PIXELS: f32 = 16_000_000.0;
const MAX_PREVIEW_SIDE: f32 = 16384.0;

pub struct RenderedFrame {
    pub image: Arc<RenderImage>,
    /// Frame size in design units, independent of the render scale.
    pub width: f32,
    pub height: f32,
    pub report: Value,
    /// Device pixels per design unit it was rendered for, or `None` if the zoom doesn't matter.
    pub resolution: Option<f32>,
    /// By layer id, as written in the frame file; empty for assets.
    pub authored_layers: HashMap<String, Vec<AuthoredProperty>>,
}

/// Where a frame is shown, so it is rasterized at that size rather than a fixed one.
#[derive(Clone, Copy, Debug)]
pub struct DisplayTarget {
    /// In logical pixels.
    pub canvas: Size<f32>,
    /// `None` while the frame is fitted to the canvas.
    pub zoom: Option<f32>,
    /// Device pixels per logical pixel.
    pub scale_factor: f32,
}

impl DisplayTarget {
    /// Device pixels per design unit at which a frame of this size is shown.
    pub fn resolution(&self, frame: Size<f32>) -> f32 {
        let zoom = self
            .zoom
            .unwrap_or_else(|| Viewport::fitting(frame, self.canvas).zoom);
        zoom * self.scale_factor
    }
}

/// Builds fresh resources so edits to tokens and assets show up; only images unchanged on disk
/// come from `image_cache`.
pub fn render_frame(
    project: &Project,
    file: &FrameFile,
    image_cache: &ImageCache,
    target: DisplayTarget,
) -> Result<RenderedFrame> {
    let resources = project.resources_with_image_cache(image_cache.clone())?;
    let scene = frames::load_scene(file, &resources)?;
    let frame_size = size(scene.width as f32, scene.height as f32);
    let resolution = target.resolution(frame_size);
    let (width, height) = raster_size(frame_size, resolution);
    let canvas = renderer::render_to_size(&scene, width, height).map_err(Failure::from)?;
    // The scene parsed it moments ago, so a failure here only loses the authored details.
    let authored_frame = fs::read_to_string(&file.path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let tokens = resources.tokens.resolved().unwrap_or_default();
    Ok(RenderedFrame {
        image: to_render_image(&canvas),
        width: frame_size.width,
        height: frame_size.height,
        report: inspect::report(&scene, None)?,
        resolution: Some(resolution),
        authored_layers: layer_details::authored_properties(&authored_frame, &tokens),
    })
}

/// The frame's size without rendering it, or `None` if its file doesn't parse.
pub fn frame_size(frame_path: &Path, tokens: &Tokens) -> Option<Size<f32>> {
    let text = fs::read_to_string(frame_path).ok()?;
    let header = document::parse_frame(&text).ok()?.header;
    let length = |value, path| tokens.number(value, path).ok().map(|length| length as f32);
    Some(size(
        length(&header.width, "width")?,
        length(&header.height, "height")?,
    ))
}

/// Reads the file again, so the export is of what's on disk at the moment it was asked for.
pub fn export_svg(project: &Project, file: &FrameFile) -> Result<String> {
    let resources = project.resources()?;
    let scene = frames::load_scene(file, &resources)?;
    Ok(svg_export::to_svg(&scene))
}

fn raster_size(frame: Size<f32>, resolution: f32) -> (u32, u32) {
    let affordable_resolution = (MAX_PREVIEW_PIXELS / (frame.width * frame.height))
        .sqrt()
        .min(MAX_PREVIEW_SIDE / frame.width.max(frame.height));
    let resolution = resolution.min(affordable_resolution);
    let pixels = |design_units: f32| (design_units * resolution).round().max(1.) as u32;
    (pixels(frame.width), pixels(frame.height))
}

pub fn to_render_image(canvas: &Pixmap) -> Arc<RenderImage> {
    bgra_render_image(canvas.width(), canvas.height(), to_straight_bgra(canvas))
}

/// `bgra` is straight, four bytes per pixel.
fn bgra_render_image(width: u32, height: u32, bgra: Vec<u8>) -> Arc<RenderImage> {
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
    fn a_fitted_frame_is_rasterized_at_its_size_on_screen() {
        let target = DisplayTarget {
            canvas: size(800., 1000.),
            zoom: None,
            scale_factor: 2.,
        };
        let frame = size(1320., 2868.);

        let resolution = target.resolution(frame);

        assert_eq!(raster_size(frame, resolution), (828, 1800));
    }

    #[test]
    fn a_deep_zoom_is_capped_to_the_preview_budget() {
        let frame = size(1320., 2868.);

        let (width, height) = raster_size(frame, 64.);

        assert!((width * height) as f32 <= MAX_PREVIEW_PIXELS * 1.001);
        assert!((height as f32 - width as f32 * 2868. / 1320.).abs() <= 1.);
    }

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
