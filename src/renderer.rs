//! Rasterizes a resolved scene. This is the only module that knows about `tiny_skia`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use tiny_skia::{
    Color as SkiaColor, FillRule, LineCap as SkiaLineCap, LineJoin as SkiaLineJoin,
    Paint as SkiaPaint, PathBuilder, Pixmap, Rect, Shader, Stroke as SkiaStroke, StrokeDash,
    Transform,
};

use crate::diagnostics::Diagnostic;
use crate::document::{LineCap, LineJoin};
use crate::scene::{Affine, Bounds, Scene, SceneKind, SceneNode, Stroke};
use crate::tokens::{Color, Paint};

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
    draw_content(node, canvas, node.transform.then(parent));
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
        SceneKind::Group { children, .. } => draw_nodes(children, canvas, world),
    }
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
fn shader(paint: &Paint, _bounds: Bounds) -> Option<Shader<'static>> {
    match paint {
        Paint::Solid { color } => Some(Shader::SolidColor(skia_color(*color))),
        Paint::LinearGradient { .. } | Paint::RadialGradient { .. } => None,
    }
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
