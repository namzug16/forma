//! Rasterizes a resolved scene. This is the only module that knows about `tiny_skia`.

use std::path::Path;

use anyhow::{Context, Result};
use tiny_skia::{
    Color as SkiaColor, ColorU8, FillRule, FilterQuality, GradientStop, LineCap as SkiaLineCap,
    LineJoin as SkiaLineJoin, LinearGradient, Mask, MaskType, Paint as SkiaPaint, PathBuilder,
    Pattern, Pixmap, PixmapPaint, Point, RadialGradient, Rect, Shader, SpreadMode,
    Stroke as SkiaStroke, StrokeDash, Transform,
};

use usvg::Tree;

use crate::assets::write_atomically;
use crate::diagnostics::Diagnostic;
use crate::document::{ImageFit, LineCap, LineJoin};
use crate::scene::{Affine, Bounds, Scene, SceneKind, SceneNode, Stroke};
use crate::tokens::{Color, ColorStop, Paint};

const MAX_OUTPUT_SIDE: f64 = 16384.0;
const MAX_OUTPUT_PIXELS: f64 = 100_000_000.0;
const INTEGRAL_SIZE_TOLERANCE: f64 = 1e-6;
const MITER_LIMIT: f32 = 4.0;
/// Control-point distance that makes a cubic Bézier approximate a quarter circle.
const CIRCLE_KAPPA: f32 = 0.552_284_8;

/// The raster size of `scene` at `scale`, which must be whole pixels within the limits.
pub fn output_size(scene: &Scene, scale: f64) -> Result<(u32, u32), Diagnostic> {
    if !(scale.is_finite() && scale > 0.0) {
        return Err(Diagnostic::new(
            "invalid_scale",
            "scale must be a positive number",
        ));
    }
    checked_raster_size(scene.width * scale, scene.height * scale)
}

/// Whole pixels within the limits, which keeps a raster allocation from failing.
pub fn checked_raster_size(width: f64, height: f64) -> Result<(u32, u32), Diagnostic> {
    if (width - width.round()).abs() > INTEGRAL_SIZE_TOLERANCE
        || (height - height.round()).abs() > INTEGRAL_SIZE_TOLERANCE
    {
        return Err(Diagnostic::new(
            "non_integral_output_size",
            format!("{width}×{height} is not a whole number of pixels"),
        ));
    }
    let (width, height) = (width.round(), height.round());
    if width < 1.0
        || height < 1.0
        || width > MAX_OUTPUT_SIDE
        || height > MAX_OUTPUT_SIDE
        || width * height > MAX_OUTPUT_PIXELS
    {
        return Err(Diagnostic::new(
            "output_too_large",
            format!(
                "{width}×{height} is outside the limits of {MAX_OUTPUT_SIDE} pixels per side \
                 and {MAX_OUTPUT_PIXELS} pixels in total"
            ),
        ));
    }
    Ok((width as u32, height as u32))
}

pub fn render(scene: &Scene, scale: f64) -> Result<Pixmap, Diagnostic> {
    let (width, height) = output_size(scene, scale)?;
    let mut canvas = Pixmap::new(width, height).expect("output_size rejects empty rasters");
    let root = Affine::scale(scale, scale);
    if let Some(background) = &scene.background {
        let bounds = Bounds::sized(scene.width, scene.height);
        fill_rect(&mut canvas, bounds, 0.0, background, root);
    }
    draw_nodes(&scene.nodes, &mut canvas, root);
    Ok(canvas)
}

pub fn write_png(canvas: &Pixmap, path: &Path) -> Result<()> {
    let encoded = canvas.encode_png().context("cannot encode PNG")?;
    path.file_name().context("output path has no file name")?;
    write_atomically(path, &encoded).with_context(|| format!("cannot write {}", path.display()))
}

pub fn copy_to_clipboard(canvas: &Pixmap) -> Result<(), arboard::Error> {
    let straight_rgba: Vec<u8> = canvas
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let pixel = pixel.demultiply();
            [pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()]
        })
        .collect();
    arboard::Clipboard::new()?.set_image(arboard::ImageData {
        width: canvas.width() as usize,
        height: canvas.height() as usize,
        bytes: straight_rgba.into(),
    })
}

/// One step of an artwork path, with coordinates in the order SVG writes them.
#[derive(Debug, Clone, Copy)]
pub enum ArtworkPathSegment {
    MoveTo(f64, f64),
    LineTo(f64, f64),
    QuadTo(f64, f64, f64, f64),
    CubicTo(f64, f64, f64, f64, f64, f64),
    Close,
}

/// The surface an artwork draws on, one shape at a time, in generation order.
pub struct ArtworkCanvas {
    pixmap: Pixmap,
    transform: Affine,
    saved_transforms: Vec<Affine>,
}

impl ArtworkCanvas {
    /// `width` and `height` must come from [`checked_raster_size`].
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            pixmap: Pixmap::new(width, height).expect("checked_raster_size rejects empty rasters"),
            transform: Affine::IDENTITY,
            saved_transforms: Vec::new(),
        }
    }

    pub fn into_pixmap(self) -> Pixmap {
        self.pixmap
    }

    /// Resets every pixel, ignoring the current transform.
    pub fn clear(&mut self, paint: Option<&Paint>) {
        self.pixmap.fill(SkiaColor::TRANSPARENT);
        if let Some(paint) = paint {
            let bounds = Bounds::sized(
                f64::from(self.pixmap.width()),
                f64::from(self.pixmap.height()),
            );
            fill_rect(&mut self.pixmap, bounds, 0.0, paint, Affine::IDENTITY);
        }
    }

    pub fn save_transform(&mut self) {
        self.saved_transforms.push(self.transform);
    }

    /// Returns false when nothing was saved.
    pub fn restore_transform(&mut self) -> bool {
        let saved = self.saved_transforms.pop();
        self.transform = saved.unwrap_or(self.transform);
        saved.is_some()
    }

    /// Later shapes go through `local` first, then the transform already in place.
    pub fn apply_transform(&mut self, local: Affine) {
        self.transform = local.then(self.transform);
    }

    pub fn rect(
        &mut self,
        bounds: Bounds,
        corner_radius: f64,
        fill: Option<&Paint>,
        stroke: Option<&Stroke>,
    ) {
        if let Some(path) = rect_path(bounds, corner_radius) {
            self.draw_shape(&path, fill, stroke);
        }
    }

    pub fn ellipse(&mut self, bounds: Bounds, fill: Option<&Paint>, stroke: Option<&Stroke>) {
        if let Some(path) = oval_path(bounds) {
            self.draw_shape(&path, fill, stroke);
        }
    }

    pub fn path(
        &mut self,
        segments: &[ArtworkPathSegment],
        fill: Option<&Paint>,
        stroke: Option<&Stroke>,
    ) {
        let mut builder = PathBuilder::new();
        for segment in segments {
            match *segment {
                ArtworkPathSegment::MoveTo(x, y) => builder.move_to(x as f32, y as f32),
                ArtworkPathSegment::LineTo(x, y) => builder.line_to(x as f32, y as f32),
                ArtworkPathSegment::QuadTo(x1, y1, x, y) => {
                    builder.quad_to(x1 as f32, y1 as f32, x as f32, y as f32);
                }
                ArtworkPathSegment::CubicTo(x1, y1, x2, y2, x, y) => builder.cubic_to(
                    x1 as f32, y1 as f32, x2 as f32, y2 as f32, x as f32, y as f32,
                ),
                ArtworkPathSegment::Close => builder.close(),
            }
        }
        if let Some(path) = builder.finish() {
            self.draw_shape(&path, fill, stroke);
        }
    }

    fn draw_shape(
        &mut self,
        path: &tiny_skia::Path,
        fill: Option<&Paint>,
        stroke: Option<&Stroke>,
    ) {
        if let Some(fill) = fill {
            let bounds = path.bounds();
            let bounds = Bounds {
                x: f64::from(bounds.x()),
                y: f64::from(bounds.y()),
                width: f64::from(bounds.width()),
                height: f64::from(bounds.height()),
            };
            fill_path(&mut self.pixmap, path, fill, bounds, self.transform);
        }
        if let Some(stroke) = stroke {
            stroke_path(&mut self.pixmap, path, stroke, self.transform);
        }
    }

    /// Asks `color_at` for straight red, green, blue and alpha in 0..=1 at every pixel, and
    /// composites the result over the canvas. Pixel coordinates ignore the current transform.
    pub fn shade<E>(
        &mut self,
        mut color_at: impl FnMut(u32, u32) -> Result<[f64; 4], E>,
    ) -> Result<(), E> {
        let width = self.pixmap.width();
        let mut layer = Pixmap::new(width, self.pixmap.height())
            .expect("a layer has the size of an existing canvas");
        let channel = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        for (index, pixel) in layer.pixels_mut().iter_mut().enumerate() {
            let index = index as u32;
            let [red, green, blue, alpha] = color_at(index % width, index / width)?;
            *pixel =
                ColorU8::from_rgba(channel(red), channel(green), channel(blue), channel(alpha))
                    .premultiply();
        }
        self.pixmap.draw_pixmap(
            0,
            0,
            layer.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        Ok(())
    }
}

fn draw_nodes(nodes: &[SceneNode], canvas: &mut Pixmap, parent: Affine) {
    for node in nodes {
        draw_node(node, canvas, parent);
    }
}

fn draw_node(node: &SceneNode, canvas: &mut Pixmap, parent: Affine) {
    if !node.visible || node.opacity == 0.0 {
        return;
    }
    let world = node.transform.then(parent);
    let (clip, group_mask) = match &node.kind {
        SceneKind::Group { clip, mask, .. } => (*clip, mask.as_deref()),
        _ => (false, None),
    };
    if node.opacity >= 1.0 && !clip && group_mask.is_none() {
        draw_content(node, canvas, world);
        return;
    }
    // Drawing into a layer makes opacity and clipping apply to the composited result, not to
    // each child, and nested clips compose because each layer is masked on its way out.
    let mut layer = Pixmap::new(canvas.width(), canvas.height())
        .expect("a layer has the size of an existing canvas");
    draw_content(node, &mut layer, world);
    if let Some(group_mask) = group_mask {
        layer.apply_mask(&alpha_mask(group_mask, canvas, world));
    }
    let mask = clip
        .then(|| clip_mask(canvas, node.bounds, world))
        .flatten();
    if clip && mask.is_none() {
        return;
    }
    let paint = PixmapPaint {
        opacity: node.opacity as f32,
        ..PixmapPaint::default()
    };
    canvas.draw_pixmap(
        0,
        0,
        layer.as_ref(),
        &paint,
        Transform::identity(),
        mask.as_ref(),
    );
}

fn alpha_mask(mask_node: &SceneNode, canvas: &Pixmap, world: Affine) -> Mask {
    let mut mask_layer = Pixmap::new(canvas.width(), canvas.height())
        .expect("a layer has the size of an existing canvas");
    draw_node(mask_node, &mut mask_layer, world);
    Mask::from_pixmap(mask_layer.as_ref(), MaskType::Alpha)
}

/// `None` for an empty clip, which hides everything inside it.
fn clip_mask(canvas: &Pixmap, bounds: Bounds, world: Affine) -> Option<Mask> {
    let path = rect_path(bounds, 0.0)?;
    let mut mask = Mask::new(canvas.width(), canvas.height())?;
    mask.fill_path(&path, FillRule::Winding, true, skia_transform(world));
    Some(mask)
}

fn draw_content(node: &SceneNode, canvas: &mut Pixmap, world: Affine) {
    let bounds = node.bounds;
    match &node.kind {
        SceneKind::Rect {
            corner_radius,
            fill,
            stroke,
        } => {
            if let Some(fill) = fill {
                fill_rect(canvas, bounds, *corner_radius, fill, world);
            }
            if let (Some(stroke), Some(path)) = (stroke, rect_path(bounds, *corner_radius)) {
                stroke_path(canvas, &path, stroke, world);
            }
        }
        SceneKind::Ellipse { fill, stroke } => {
            let Some(path) = oval_path(bounds) else {
                return;
            };
            if let Some(fill) = fill {
                fill_path(canvas, &path, fill, bounds, world);
            }
            if let Some(stroke) = stroke {
                stroke_path(canvas, &path, stroke, world);
            }
        }
        SceneKind::Line { stroke } => {
            let mut builder = PathBuilder::new();
            builder.move_to(bounds.x as f32, bounds.y as f32);
            builder.line_to(
                (bounds.x + bounds.width) as f32,
                (bounds.y + bounds.height) as f32,
            );
            if let Some(path) = builder.finish() {
                stroke_path(canvas, &path, stroke, world);
            }
        }
        SceneKind::Text { outlines, fill, .. } => {
            if let Some(outlines) = outlines {
                fill_path(canvas, outlines, fill, bounds, world);
            }
        }
        SceneKind::Image { image, fit, .. } => draw_image(canvas, image, *fit, bounds, world),
        SceneKind::Svg { tree, .. } => draw_svg(canvas, tree, bounds, world),
        SceneKind::Group { children, .. } => draw_nodes(children, canvas, world),
    }
}

fn draw_image(canvas: &mut Pixmap, pixels: &Pixmap, fit: ImageFit, bounds: Bounds, world: Affine) {
    let (image_width, image_height) = (f64::from(pixels.width()), f64::from(pixels.height()));
    let (scale_x, scale_y) = (bounds.width / image_width, bounds.height / image_height);
    let (scale_x, scale_y) = match fit {
        ImageFit::Fill => (scale_x, scale_y),
        ImageFit::Cover => (scale_x.max(scale_y), scale_x.max(scale_y)),
        ImageFit::Contain => (scale_x.min(scale_y), scale_x.min(scale_y)),
    };
    let (drawn_width, drawn_height) = (image_width * scale_x, image_height * scale_y);
    let (left, top) = (
        bounds.x + (bounds.width - drawn_width) / 2.0,
        bounds.y + (bounds.height - drawn_height) / 2.0,
    );
    // Cover overflows the box and contain letterboxes it; only their overlap is painted.
    let visible = Bounds {
        x: left.max(bounds.x),
        y: top.max(bounds.y),
        width: drawn_width.min(bounds.width),
        height: drawn_height.min(bounds.height),
    };
    let Some(path) = rect_path(visible, 0.0) else {
        return;
    };
    let image_to_box = Transform::from_row(
        scale_x as f32,
        0.0,
        0.0,
        scale_y as f32,
        left as f32,
        top as f32,
    );
    let paint = SkiaPaint {
        // shortcut: bicubic undersamples large reductions, add mipmaps if downscaled photos alias
        shader: Pattern::new(
            pixels.as_ref(),
            SpreadMode::Pad,
            FilterQuality::Bicubic,
            1.0,
            image_to_box,
        ),
        anti_alias: true,
        ..SkiaPaint::default()
    };
    canvas.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        skia_transform(world),
        None,
    );
}

fn draw_svg(canvas: &mut Pixmap, tree: &Tree, bounds: Bounds, world: Affine) {
    let transform = skia_transform(svg_to_box(tree, bounds).then(world));
    resvg::render(tree, transform, &mut canvas.as_mut());
}

/// Fits the SVG inside the box without distorting it, centered.
pub fn svg_to_box(tree: &Tree, bounds: Bounds) -> Affine {
    let (svg_width, svg_height) = (
        f64::from(tree.size().width()),
        f64::from(tree.size().height()),
    );
    let scale = (bounds.width / svg_width).min(bounds.height / svg_height);
    Affine::scale(scale, scale).then(Affine::translate(
        bounds.x + (bounds.width - svg_width * scale) / 2.0,
        bounds.y + (bounds.height - svg_height * scale) / 2.0,
    ))
}

fn skia_rect(bounds: Bounds) -> Option<Rect> {
    Rect::from_xywh(
        bounds.x as f32,
        bounds.y as f32,
        bounds.width as f32,
        bounds.height as f32,
    )
}

fn rect_path(bounds: Bounds, corner_radius: f64) -> Option<tiny_skia::Path> {
    let rect = skia_rect(bounds)?;
    if corner_radius <= 0.0 {
        return Some(PathBuilder::from_rect(rect));
    }
    let radius = (corner_radius as f32)
        .min(rect.width() / 2.0)
        .min(rect.height() / 2.0);
    let handle = radius * (1.0 - CIRCLE_KAPPA);
    let (left, top, right, bottom) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    let mut builder = PathBuilder::new();
    builder.move_to(left + radius, top);
    builder.line_to(right - radius, top);
    builder.cubic_to(
        right - handle,
        top,
        right,
        top + handle,
        right,
        top + radius,
    );
    builder.line_to(right, bottom - radius);
    builder.cubic_to(
        right,
        bottom - handle,
        right - handle,
        bottom,
        right - radius,
        bottom,
    );
    builder.line_to(left + radius, bottom);
    builder.cubic_to(
        left + handle,
        bottom,
        left,
        bottom - handle,
        left,
        bottom - radius,
    );
    builder.line_to(left, top + radius);
    builder.cubic_to(left, top + handle, left + handle, top, left + radius, top);
    builder.close();
    builder.finish()
}

fn oval_path(bounds: Bounds) -> Option<tiny_skia::Path> {
    PathBuilder::from_oval(skia_rect(bounds)?)
}

fn skia_transform(transform: Affine) -> Transform {
    Transform::from_row(
        transform.a as f32,
        transform.b as f32,
        transform.c as f32,
        transform.d as f32,
        transform.e as f32,
        transform.f as f32,
    )
}

fn skia_color(color: Color) -> SkiaColor {
    SkiaColor::from_rgba8(color.red, color.green, color.blue, color.alpha)
}

fn fill_rect(
    canvas: &mut Pixmap,
    bounds: Bounds,
    corner_radius: f64,
    paint: &Paint,
    world: Affine,
) {
    if let Some(path) = rect_path(bounds, corner_radius) {
        fill_path(canvas, &path, paint, bounds, world);
    }
}

fn fill_path(
    canvas: &mut Pixmap,
    path: &tiny_skia::Path,
    paint: &Paint,
    bounds: Bounds,
    world: Affine,
) {
    let Some(shader) = shader(paint, bounds) else {
        return;
    };
    let skia_paint = SkiaPaint {
        shader,
        anti_alias: true,
        ..SkiaPaint::default()
    };
    canvas.fill_path(
        path,
        &skia_paint,
        FillRule::Winding,
        skia_transform(world),
        None,
    );
}

/// The gradient's start and end. CSS gradient length: stops 0 and 1 land exactly on the box's far
/// corners.
pub fn linear_gradient_line(angle: f64, bounds: Bounds) -> ((f64, f64), (f64, f64)) {
    let (sine, cosine) = angle.to_radians().sin_cos();
    let half_length = (bounds.width * sine.abs() + bounds.height * cosine.abs()) / 2.0;
    let (center_x, center_y) = (
        bounds.x + bounds.width / 2.0,
        bounds.y + bounds.height / 2.0,
    );
    let (offset_x, offset_y) = (sine * half_length, -cosine * half_length);
    (
        (center_x - offset_x, center_y - offset_y),
        (center_x + offset_x, center_y + offset_y),
    )
}

/// `None` when the paint cannot produce color for this box, such as a gradient on a zero-size one.
fn shader(paint: &Paint, bounds: Bounds) -> Option<Shader<'static>> {
    if !matches!(paint, Paint::Solid { .. }) && (bounds.width <= 0.0 || bounds.height <= 0.0) {
        return None;
    }
    match paint {
        Paint::Solid { color } => Some(Shader::SolidColor(skia_color(*color))),
        Paint::LinearGradient { angle, stops } => {
            let ((start_x, start_y), (end_x, end_y)) = linear_gradient_line(*angle, bounds);
            LinearGradient::new(
                Point::from_xy(start_x as f32, start_y as f32),
                Point::from_xy(end_x as f32, end_y as f32),
                skia_stops(stops),
                SpreadMode::Pad,
                Transform::identity(),
            )
        }
        Paint::RadialGradient {
            center,
            radius,
            stops,
        } => {
            // A unit circle stretched per axis, so center and radius share one normalization.
            let unit_circle_to_box = Transform::from_scale(
                (radius * bounds.width) as f32,
                (radius * bounds.height) as f32,
            )
            .post_translate(
                (bounds.x + center[0] * bounds.width) as f32,
                (bounds.y + center[1] * bounds.height) as f32,
            );
            RadialGradient::new(
                Point::zero(),
                0.0,
                Point::zero(),
                1.0,
                skia_stops(stops),
                SpreadMode::Pad,
                unit_circle_to_box,
            )
        }
    }
}

fn skia_stops(stops: &[ColorStop]) -> Vec<GradientStop> {
    stops
        .iter()
        .map(|stop| GradientStop::new(stop.offset as f32, skia_color(stop.color)))
        .collect()
}

fn stroke_path(canvas: &mut Pixmap, path: &tiny_skia::Path, stroke: &Stroke, world: Affine) {
    // tiny-skia treats a zero width as a hairline; here it means no stroke.
    if stroke.width == 0.0 {
        return;
    }
    let mut color = skia_color(stroke.color);
    color.apply_opacity(stroke.opacity as f32);
    let dash_lengths = stroke.dash.iter().map(|length| *length as f32);
    // An odd list repeats to alternate dashes and gaps, as SVG does.
    let dash_lengths: Vec<f32> = if stroke.dash.len().is_multiple_of(2) {
        dash_lengths.collect()
    } else {
        dash_lengths.clone().chain(dash_lengths).collect()
    };
    let skia_stroke = SkiaStroke {
        width: stroke.width as f32,
        miter_limit: MITER_LIMIT,
        line_cap: match stroke.cap {
            LineCap::Butt => SkiaLineCap::Butt,
            LineCap::Round => SkiaLineCap::Round,
            LineCap::Square => SkiaLineCap::Square,
        },
        line_join: match stroke.join {
            LineJoin::Miter => SkiaLineJoin::Miter,
            LineJoin::Round => SkiaLineJoin::Round,
            LineJoin::Bevel => SkiaLineJoin::Bevel,
        },
        dash: StrokeDash::new(dash_lengths, 0.0),
    };
    let paint = SkiaPaint {
        shader: Shader::SolidColor(color),
        anti_alias: true,
        ..SkiaPaint::default()
    };
    canvas.stroke_path(path, &paint, &skia_stroke, skia_transform(world), None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use crate::assets::Assets;
    use crate::document::parse_frame;
    use crate::scene::{Resources, build};
    use crate::text::FontLibrary;
    use crate::tokens::Tokens;

    fn scene_json(frame_json: &str, tokens_json: &str) -> Scene {
        let resources = Resources {
            tokens: Tokens::parse(tokens_json).unwrap(),
            assets: Assets::new("/no/assets".into()),
            fonts: FontLibrary::default(),
        };
        build(&parse_frame(frame_json).unwrap(), &resources).unwrap()
    }

    fn render_json(frame_json: &str, tokens_json: &str) -> Pixmap {
        render(&scene_json(frame_json, tokens_json), 1.0).unwrap()
    }

    fn frame_with(children: &str) -> String {
        format!(
            r#"{{ "version": 1, "id": "f", "width": 100, "height": 100, "children": [{children}] }}"#
        )
    }

    /// A 10×10 red square with this id at (`x`, `y`).
    fn red_square(id: &str, x: u32, y: u32) -> String {
        format!(
            r##"{{ "id": "{id}", "type": "rect", "x": {x}, "y": {y}, "width": 10, "height": 10,
                  "fill": {{ "color": "#FF0000" }} }}"##
        )
    }

    fn group_children(scene: &Scene) -> &[SceneNode] {
        let SceneKind::Group { children, .. } = &scene.nodes[0].kind else {
            panic!("the first node is a group");
        };
        children
    }

    const RED: [u8; 4] = [255, 0, 0, 255];
    const EMPTY: [u8; 4] = [0, 0, 0, 0];

    /// Straight (not premultiplied) RGBA at a pixel.
    fn pixel(canvas: &Pixmap, x: u32, y: u32) -> [u8; 4] {
        let color = canvas.pixel(x, y).unwrap().demultiply();
        [color.red(), color.green(), color.blue(), color.alpha()]
    }

    fn assert_near(actual: [u8; 4], expected: [u8; 4]) {
        let is_close = actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual.abs_diff(expected) <= 4);
        assert!(is_close, "{actual:?} is not close to {expected:?}");
    }

    #[test]
    fn linear_gradient_interpolates_three_stops_with_tokens_and_transparency() {
        let canvas = render_json(
            r##"{ "version": 1, "id": "f", "width": 200, "height": 10, "children": [
                { "id": "g", "type": "rect", "width": 200, "height": 10, "fill": {
                    "type": "linear-gradient", "angle": 90, "stops": [
                        { "offset": 0, "color": "$colors.red" },
                        { "offset": 0.5, "color": "#00FF00" },
                        { "offset": 1, "color": "#0000FF00" } ] } } ] }"##,
            r##"{ "colors": { "red": "#FF0000" } }"##,
        );

        assert_near(pixel(&canvas, 0, 5), [255, 0, 0, 255]);
        assert_near(pixel(&canvas, 50, 5), [127, 127, 0, 255]);
        assert_near(pixel(&canvas, 100, 5), [0, 255, 0, 255]);
        // Halfway to a transparent stop: color and alpha interpolate independently, in sRGB.
        assert_near(pixel(&canvas, 150, 5), [0, 127, 127, 127]);
        assert!(canvas.pixel(199, 5).unwrap().alpha() < 4);
    }

    #[test]
    fn gradient_angle_zero_points_up_and_stays_in_object_space_when_rotated() {
        let canvas = render_json(
            r##"{ "version": 1, "id": "f", "width": 100, "height": 100, "children": [
                { "id": "g", "type": "rect", "width": 100, "height": 100, "rotation": 90,
                  "anchor": [0.5, 0.5], "x": 50, "y": 50, "fill": {
                    "type": "linear-gradient", "angle": 0, "stops": [
                        { "offset": 0, "color": "#000000" },
                        { "offset": 1, "color": "#FFFFFF" } ] } } ] }"##,
            "{}",
        );

        // Up in object space is right on screen after a clockwise quarter turn.
        assert_near(pixel(&canvas, 98, 50), [253, 253, 253, 255]);
        assert_near(pixel(&canvas, 1, 50), [3, 3, 3, 255]);
    }

    #[test]
    fn radial_radius_is_normalized_per_axis() {
        let canvas = render_json(
            r##"{ "version": 1, "id": "f", "width": 200, "height": 100, "children": [
                { "id": "g", "type": "rect", "width": 200, "height": 100, "fill": {
                    "type": "radial-gradient", "center": [0.5, 0.5], "radius": 0.5, "stops": [
                        { "offset": 0, "color": "#000000" },
                        { "offset": 1, "color": "#FFFFFF" } ] } } ] }"##,
            "{}",
        );

        assert_near(pixel(&canvas, 100, 50), [0, 0, 0, 255]);
        // Halfway out along each axis is equally far through the gradient.
        assert_near(pixel(&canvas, 150, 50), [127, 127, 127, 255]);
        assert_near(pixel(&canvas, 100, 75), [127, 127, 127, 255]);
    }

    #[test]
    fn group_opacity_applies_once_to_the_composited_children() {
        let canvas = render_json(
            r##"{ "version": 1, "id": "f", "width": 30, "height": 10, "children": [
                { "id": "group", "type": "group", "opacity": 0.5, "children": [
                    { "id": "a", "type": "rect", "width": 20, "height": 10,
                      "fill": { "color": "#FF0000" } },
                    { "id": "b", "type": "rect", "x": 10, "width": 20, "height": 10,
                      "fill": { "color": "#FF0000" } } ] } ] }"##,
            "{}",
        );

        // Per-child opacity would make the overlap at x=15 denser than x=5.
        assert_eq!(pixel(&canvas, 15, 5), pixel(&canvas, 5, 5));
        assert_near(pixel(&canvas, 15, 5), [255, 0, 0, 127]);
    }

    #[test]
    fn rotated_children_are_clipped_by_groups_and_the_frame() {
        let canvas = render_json(
            r##"{ "version": 1, "id": "f", "width": 100, "height": 100, "children": [
                { "id": "diamond", "type": "group", "x": 50, "y": 50, "width": 40, "height": 40,
                  "anchor": [0.5, 0.5], "rotation": 45, "clip": true, "children": [
                    { "id": "flood", "type": "rect", "x": -100, "y": -100,
                      "width": 300, "height": 300, "fill": { "color": "#00FF00" } } ] },
                { "id": "overflowing", "type": "rect", "x": 100, "y": 0, "width": 60, "height": 60,
                  "anchor": [0.5, 0.5], "rotation": 45, "fill": { "color": "#0000FF" } } ] }"##,
            "{}",
        );

        assert_eq!((canvas.width(), canvas.height()), (100, 100));
        assert_near(pixel(&canvas, 50, 25), [0, 255, 0, 255]);
        // Inside the group's unrotated box, but outside the rotated clip.
        assert_eq!(pixel(&canvas, 33, 33)[3], 0);
        assert_near(pixel(&canvas, 99, 1), [0, 0, 255, 255]);
    }

    #[test]
    fn children_are_positioned_relative_to_their_group() {
        let square = red_square("square", 5, 5);
        let frame = frame_with(&format!(
            r#"{{ "id": "group", "type": "group", "x": 20, "y": 10, "children": [{square}] }}"#
        ));
        let canvas = render_json(&frame, "{}");

        assert_eq!(pixel(&canvas, 30, 20), RED);
        assert_eq!(pixel(&canvas, 10, 10), EMPTY);
    }

    #[test]
    fn moving_a_group_moves_its_children_without_changing_their_transforms() {
        let squares = format!("{},{}", red_square("a", 0, 0), red_square("b", 20, 0));
        let at = |x: u32| {
            frame_with(&format!(
                r#"{{ "id": "group", "type": "group", "x": {x}, "children": [{squares}] }}"#
            ))
        };
        let (before, after) = (scene_json(&at(0), "{}"), scene_json(&at(50), "{}"));

        for (child_before, child_after) in
            group_children(&before).iter().zip(group_children(&after))
        {
            assert_eq!(child_before.transform, child_after.transform);
        }
        let canvas = render(&after, 1.0).unwrap();
        assert_eq!(pixel(&canvas, 5, 5), EMPTY);
        assert_eq!(pixel(&canvas, 55, 5), RED);
        assert_eq!(pixel(&canvas, 75, 5), RED);
    }

    #[test]
    fn a_group_rotates_its_children_around_the_pivot() {
        let square = red_square("square", 0, 0);
        let frame = frame_with(&format!(
            r#"{{ "id": "group", "type": "group", "x": 50, "y": 50, "width": 20, "height": 20,
                  "anchor": [0.5, 0.5], "rotation": 90, "children": [{square}] }}"#
        ));
        let canvas = render_json(&frame, "{}");

        // The top-left quadrant of the group turns to the top-right.
        assert_eq!(pixel(&canvas, 55, 45), RED);
        assert_eq!(pixel(&canvas, 45, 45), EMPTY);
    }

    #[test]
    fn scaling_a_group_scales_children_and_the_gaps_between_them() {
        let squares = format!("{},{}", red_square("a", 0, 0), red_square("b", 20, 0));
        let frame = frame_with(&format!(
            r#"{{ "id": "group", "type": "group", "scaleX": 2, "scaleY": 2,
                  "children": [{squares}] }}"#
        ));
        let canvas = render_json(&frame, "{}");

        assert_eq!(pixel(&canvas, 15, 15), RED);
        assert_eq!(pixel(&canvas, 30, 5), EMPTY);
        assert_eq!(pixel(&canvas, 55, 15), RED);
    }

    #[test]
    fn nested_groups_compose_their_transforms() {
        let square = red_square("square", 0, 0);
        let frame = frame_with(&format!(
            r#"{{ "id": "outer", "type": "group", "x": 50, "rotation": 90, "children": [
                {{ "id": "inner", "type": "group", "x": 10, "scaleX": 2, "scaleY": 2,
                   "children": [{square}] }} ] }}"#
        ));
        let canvas = render_json(&frame, "{}");

        // Inner places the square over x 10..30, y 0..20; outer turns that to x 30..50, y 10..30.
        assert_eq!(pixel(&canvas, 40, 20), RED);
        assert_eq!(pixel(&canvas, 15, 5), EMPTY);
    }

    #[test]
    fn changing_one_child_leaves_its_siblings_alone() {
        let with_second_at = |x: u32| {
            let squares = format!("{},{}", red_square("a", 0, 0), red_square("b", x, 0));
            frame_with(&format!(
                r#"{{ "id": "group", "type": "group", "children": [{squares}] }}"#
            ))
        };
        let before = scene_json(&with_second_at(20), "{}");
        let after = scene_json(&with_second_at(60), "{}");

        assert_eq!(
            group_children(&before)[0].transform,
            group_children(&after)[0].transform
        );
        let canvas = render(&after, 1.0).unwrap();
        assert_eq!(pixel(&canvas, 5, 5), RED);
        assert_eq!(pixel(&canvas, 25, 5), EMPTY);
        assert_eq!(pixel(&canvas, 65, 5), RED);
    }

    #[test]
    fn a_hidden_group_hides_descendants_that_are_visible_themselves() {
        let square = red_square("square", 0, 0);
        let frame = frame_with(&format!(
            r#"{{ "id": "outer", "type": "group", "visible": false, "children": [
                {{ "id": "inner", "type": "group", "visible": true, "children": [{square}] }} ] }}"#
        ));
        let canvas = render_json(&frame, "{}");

        assert!(canvas.pixels().iter().all(|pixel| pixel.alpha() == 0));
    }

    #[test]
    fn nested_clips_keep_only_their_overlap() {
        let flood = r##"{ "id": "flood", "type": "rect", "width": 100, "height": 100,
                          "fill": { "color": "#FF0000" } }"##;
        let frame = frame_with(&format!(
            r#"{{ "id": "outer", "type": "group", "width": 20, "height": 100, "clip": true,
                  "children": [
                {{ "id": "inner", "type": "group", "x": 10, "width": 20, "height": 100,
                   "clip": true, "children": [{flood}] }} ] }}"#
        ));
        let canvas = render_json(&frame, "{}");

        assert_eq!(pixel(&canvas, 5, 50), EMPTY);
        assert_eq!(pixel(&canvas, 15, 50), RED);
        assert_eq!(pixel(&canvas, 25, 50), EMPTY);
    }

    #[test]
    fn a_mask_moves_with_its_group_and_multiplies_its_alpha() {
        let frame = frame_with(
            r##"{ "id": "group", "type": "group", "x": 20, "y": 20,
                  "mask": { "id": "circle", "type": "ellipse", "width": 40, "height": 40,
                            "opacity": 0.5, "fill": { "color": "#FFFFFF" } },
                  "children": [
                    { "id": "flood", "type": "rect", "x": -20, "y": -20,
                      "width": 100, "height": 100, "fill": { "color": "#FF0000" } } ] }"##,
        );
        let canvas = render_json(&frame, "{}");

        assert_near(pixel(&canvas, 40, 40), [255, 0, 0, 127]);
        // Inside the circle's box, outside the circle.
        assert_eq!(pixel(&canvas, 21, 21), EMPTY);
        assert_eq!(pixel(&canvas, 70, 40), EMPTY);
    }

    #[test]
    fn only_groups_accept_a_mask() {
        let frame = frame_with(
            r#"{ "id": "r", "type": "rect", "width": 1, "height": 1,
                 "mask": { "id": "m", "type": "rect", "width": 1, "height": 1 } }"#,
        );

        assert_eq!(
            parse_frame(&frame).unwrap_err().0[0].code,
            "unsupported_feature"
        );
    }

    #[test]
    fn svg_color_replaces_current_color_but_not_hard_coded_colors() {
        let directory = tempfile::tempdir().unwrap();
        // Shaped like an Iconify download, whose size is in `em`.
        fs::write(
            directory.path().join("icon.svg"),
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="1em" height="1em" viewBox="0 0 24 24">
                <rect width="12" height="24" fill="currentColor"/>
                <rect x="12" width="12" height="24" fill="#00FF00"/></svg>"##,
        )
        .unwrap();
        let resources = Resources {
            tokens: Tokens::parse(r##"{ "colors": { "icon": "#0000FF80" } }"##).unwrap(),
            assets: Assets::new(directory.path().into()),
            fonts: FontLibrary::default(),
        };
        let frame = frame_with(
            r#"{ "id": "icon", "type": "svg", "src": "icon.svg", "width": 100, "height": 100,
                 "color": "$colors.icon" }"#,
        );
        let scene = build(&parse_frame(&frame).unwrap(), &resources).unwrap();
        let canvas = render(&scene, 1.0).unwrap();

        assert_near(pixel(&canvas, 25, 50), [0, 0, 255, 128]);
        assert_near(pixel(&canvas, 75, 50), [0, 255, 0, 255]);
    }

    #[test]
    fn later_children_draw_over_earlier_ones() {
        let frame = frame_with(&format!(
            r##"{{ "id": "group", "type": "group", "children": [
                {{ "id": "under", "type": "rect", "width": 10, "height": 10,
                   "fill": {{ "color": "#0000FF" }} }},
                {} ] }}"##,
            red_square("over", 0, 0)
        ));

        assert_eq!(pixel(&render_json(&frame, "{}"), 5, 5), RED);
    }
}
