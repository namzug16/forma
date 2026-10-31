//! Rasterizes a resolved scene. This is the only module that knows about `tiny_skia`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use tiny_skia::{
    Color as SkiaColor, FillRule, FilterQuality, GradientStop, IntSize, LineCap as SkiaLineCap,
    LineJoin as SkiaLineJoin, LinearGradient, Mask, Paint as SkiaPaint, PathBuilder, Pattern,
    Pixmap, PixmapPaint, Point, RadialGradient, Rect, Shader, SpreadMode, Stroke as SkiaStroke,
    StrokeDash, Transform,
};

use usvg::Tree;

use crate::assets::DecodedImage;
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
    let (width, height) = (scene.width * scale, scene.height * scale);
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

/// Writes to a sibling temporary file first, so a failure never leaves a partial image.
pub fn write_png(canvas: &Pixmap, path: &Path) -> Result<()> {
    let encoded = canvas.encode_png().context("cannot encode PNG")?;
    let parent = path
        .parent()
        .context("output path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    let file_name = path.file_name().context("output path has no file name")?;
    let temporary_path = parent.join(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id()
    ));
    let result = fs::write(&temporary_path, encoded)
        .and_then(|()| fs::rename(&temporary_path, path))
        .with_context(|| format!("cannot write {}", path.display()));
    if result.is_err() {
        // Nothing more to do if cleanup fails too; the write error is the one to report.
        let _ = fs::remove_file(&temporary_path);
    }
    result
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
    let clip = matches!(node.kind, SceneKind::Group { clip: true, .. });
    if node.opacity >= 1.0 && !clip {
        draw_content(node, canvas, world);
        return;
    }
    // Drawing into a layer makes opacity and clipping apply to the composited result, not to
    // each child, and nested clips compose because each layer is masked on its way out.
    let mut layer = Pixmap::new(canvas.width(), canvas.height())
        .expect("a layer has the size of an existing canvas");
    draw_content(node, &mut layer, world);
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
        SceneKind::Image { image, fit, .. } => draw_image(canvas, image, *fit, bounds, world),
        SceneKind::Svg { tree, .. } => draw_svg(canvas, tree, bounds, world),
        SceneKind::Group { children, .. } => draw_nodes(children, canvas, world),
    }
}

fn draw_image(
    canvas: &mut Pixmap,
    image: &DecodedImage,
    fit: ImageFit,
    bounds: Bounds,
    world: Affine,
) {
    let Some(pixels) = premultiplied_pixmap(image) else {
        return;
    };
    let (image_width, image_height) = (f64::from(image.width), f64::from(image.height));
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

/// Fits the SVG inside the box without distorting it, centered.
fn draw_svg(canvas: &mut Pixmap, tree: &Tree, bounds: Bounds, world: Affine) {
    let (svg_width, svg_height) = (
        f64::from(tree.size().width()),
        f64::from(tree.size().height()),
    );
    let scale = (bounds.width / svg_width).min(bounds.height / svg_height);
    let svg_to_box = Affine::scale(scale, scale).then(Affine::translate(
        bounds.x + (bounds.width - svg_width * scale) / 2.0,
        bounds.y + (bounds.height - svg_height * scale) / 2.0,
    ));
    let transform = skia_transform(svg_to_box.then(world));
    resvg::render(tree, transform, &mut canvas.as_mut());
}

fn premultiplied_pixmap(image: &DecodedImage) -> Option<Pixmap> {
    let premultiply =
        |channel: u8, alpha: u8| ((u16::from(channel) * u16::from(alpha) + 127) / 255) as u8;
    let mut premultiplied = image.rgba.clone();
    for pixel in premultiplied.chunks_exact_mut(4) {
        let alpha = pixel[3];
        pixel[0] = premultiply(pixel[0], alpha);
        pixel[1] = premultiply(pixel[1], alpha);
        pixel[2] = premultiply(pixel[2], alpha);
    }
    Pixmap::from_vec(premultiplied, IntSize::from_wh(image.width, image.height)?)
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

/// `None` when the paint cannot produce color for this box, such as a gradient on a zero-size one.
fn shader(paint: &Paint, bounds: Bounds) -> Option<Shader<'static>> {
    if !matches!(paint, Paint::Solid { .. }) && (bounds.width <= 0.0 || bounds.height <= 0.0) {
        return None;
    }
    match paint {
        Paint::Solid { color } => Some(Shader::SolidColor(skia_color(*color))),
        Paint::LinearGradient { angle, stops } => {
            // CSS gradient length: stops 0 and 1 land exactly on the box's far corners.
            let (sine, cosine) = angle.to_radians().sin_cos();
            let half_length = (bounds.width * sine.abs() + bounds.height * cosine.abs()) / 2.0;
            let (center_x, center_y) = (
                bounds.x + bounds.width / 2.0,
                bounds.y + bounds.height / 2.0,
            );
            let (offset_x, offset_y) = (sine * half_length, -cosine * half_length);
            LinearGradient::new(
                Point::from_xy((center_x - offset_x) as f32, (center_y - offset_y) as f32),
                Point::from_xy((center_x + offset_x) as f32, (center_y + offset_y) as f32),
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
    let dash_lengths: Vec<f32> = if stroke.dash.len() % 2 == 0 {
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
    use crate::assets::Assets;
    use crate::document::parse_frame;
    use crate::scene::{Resources, build};
    use crate::tokens::Tokens;

    fn render_json(frame_json: &str, tokens_json: &str) -> Pixmap {
        let resources = Resources {
            tokens: Tokens::parse(tokens_json).unwrap(),
            assets: Assets::new("/no/assets".into()),
        };
        let scene = build(&parse_frame(frame_json).unwrap(), &resources).unwrap();
        render(&scene, 1.0).unwrap()
    }

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
}
