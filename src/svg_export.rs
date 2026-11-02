//! Writes a resolved scene as SVG markup. Clips and masks follow the shapes Figma's own SVG
//! export uses, so pasting the result into Figma has the best chance of keeping them.

use std::fmt::Write as _;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use tiny_skia::PathSegment;
use usvg::{Tree, WriteOptions};

use crate::document::{ImageFit, LineCap, LineJoin};
use crate::renderer::{linear_gradient_line, premultiplied_pixmap, svg_to_box};
use crate::scene::{Affine, Bounds, Scene, SceneKind, SceneNode, Stroke};
use crate::tokens::{Color, ColorStop, Paint};

/// The markup is self-contained: images are embedded and text is outlined, so it needs neither
/// the project's assets nor its fonts.
pub fn to_svg(scene: &Scene) -> String {
    let mut writer = SvgWriter::default();
    let mut body = String::new();
    if let Some(background) = &scene.background {
        let fill = writer.fill_attributes(background, Bounds::sized(scene.width, scene.height));
        write!(
            body,
            r#"<rect width="{}" height="{}" {fill}/>"#,
            scene.width, scene.height
        )
        .expect("writing to a string cannot fail");
    }
    writer.write_nodes(&scene.nodes, &mut body);
    format!(
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" "#,
            r#"xmlns:xlink="http://www.w3.org/1999/xlink" "#,
            r#"width="{width}" height="{height}" viewBox="0 0 {width} {height}" fill="none">"#,
            "{body}<defs>{defs}</defs></svg>"
        ),
        width = scene.width,
        height = scene.height,
        body = body,
        defs = writer.definitions,
    )
}

#[derive(Default)]
struct SvgWriter {
    definitions: String,
    next_id: usize,
}

impl SvgWriter {
    fn unique_id(&mut self, kind: &str) -> String {
        self.next_id += 1;
        format!("forma-{kind}-{}", self.next_id)
    }

    fn write_nodes(&mut self, nodes: &[SceneNode], output: &mut String) {
        for node in nodes {
            self.write_node(node, output);
        }
    }

    fn write_node(&mut self, node: &SceneNode, output: &mut String) {
        if !node.visible || node.opacity == 0.0 {
            return;
        }
        let bounds = node.bounds;
        let id = escape_xml(&node.id);
        let opacity = opacity_attribute(node.opacity);
        let transform = transform_attribute(node.transform);
        let common = format!(r#"id="{id}"{transform}{opacity}"#);
        match &node.kind {
            SceneKind::Rect {
                corner_radius,
                fill,
                stroke,
            } => {
                let paint = self.paint_attributes(fill.as_ref(), stroke.as_ref(), bounds);
                let radius = if *corner_radius > 0.0 {
                    format!(r#" rx="{corner_radius}""#)
                } else {
                    String::new()
                };
                let Bounds {
                    x,
                    y,
                    width,
                    height,
                } = bounds;
                push(
                    output,
                    format!(
                        r#"<rect {common} x="{x}" y="{y}" width="{width}" height="{height}"{radius} {paint}/>"#
                    ),
                );
            }
            SceneKind::Ellipse { fill, stroke } => {
                let paint = self.paint_attributes(fill.as_ref(), stroke.as_ref(), bounds);
                let (radius_x, radius_y) = (bounds.width / 2.0, bounds.height / 2.0);
                let (center_x, center_y) = (bounds.x + radius_x, bounds.y + radius_y);
                push(
                    output,
                    format!(
                        r#"<ellipse {common} cx="{center_x}" cy="{center_y}" rx="{radius_x}" ry="{radius_y}" {paint}/>"#
                    ),
                );
            }
            SceneKind::Line { stroke } => {
                let (x2, y2) = (bounds.x + bounds.width, bounds.y + bounds.height);
                let stroke = stroke_attributes(stroke);
                push(
                    output,
                    format!(
                        r#"<line {common} x1="{}" y1="{}" x2="{x2}" y2="{y2}" {stroke}/>"#,
                        bounds.x, bounds.y
                    ),
                );
            }
            SceneKind::Text { outlines, fill, .. } => {
                if let Some(outlines) = outlines {
                    let fill = self.fill_attributes(fill, bounds);
                    let data = path_data(outlines);
                    push(output, format!(r#"<path {common} d="{data}" {fill}/>"#));
                }
            }
            SceneKind::Image { image, fit, .. } => {
                let Some(png) = premultiplied_pixmap(image).and_then(|p| p.encode_png().ok())
                else {
                    return;
                };
                let aspect_ratio = match fit {
                    ImageFit::Fill => "none",
                    ImageFit::Cover => "xMidYMid slice",
                    ImageFit::Contain => "xMidYMid meet",
                };
                let Bounds {
                    x,
                    y,
                    width,
                    height,
                } = bounds;
                let data = BASE64.encode(png);
                push(
                    output,
                    format!(
                        r#"<image {common} x="{x}" y="{y}" width="{width}" height="{height}" preserveAspectRatio="{aspect_ratio}" xlink:href="data:image/png;base64,{data}"/>"#
                    ),
                );
            }
            SceneKind::Svg { tree, .. } => {
                let placed = transform_attribute(svg_to_box(tree, bounds).then(node.transform));
                let content = self.embedded_svg_content(tree);
                push(
                    output,
                    format!(r#"<g id="{id}"{placed}{opacity}>{content}</g>"#),
                );
            }
            SceneKind::Group {
                children,
                clip,
                mask,
            } => {
                let mut group = common;
                if *clip {
                    let clip_id = self.unique_id("clip");
                    let Bounds {
                        x,
                        y,
                        width,
                        height,
                    } = bounds;
                    let _ = write!(
                        self.definitions,
                        r#"<clipPath id="{clip_id}"><rect x="{x}" y="{y}" width="{width}" height="{height}"/></clipPath>"#
                    );
                    let _ = write!(group, r#" clip-path="url(#{clip_id})""#);
                }
                if let Some(mask) = mask {
                    let mask_id = self.unique_id("mask");
                    let mut content = String::new();
                    self.write_node(mask, &mut content);
                    let region = mask_region(mask);
                    // Outside `<defs>`, where Figma has been reported to drop masks.
                    push(
                        output,
                        format!(
                            r#"<mask id="{mask_id}" style="mask-type:alpha" maskUnits="userSpaceOnUse" x="{}" y="{}" width="{}" height="{}">{content}</mask>"#,
                            region.x, region.y, region.width, region.height
                        ),
                    );
                    let _ = write!(group, r#" mask="url(#{mask_id})""#);
                }
                push(output, format!("<g {group}>"));
                self.write_nodes(children, output);
                output.push_str("</g>");
            }
        }
    }

    /// The inside of the embedded document, with ids that can't collide with another embed.
    fn embedded_svg_content(&mut self, tree: &Tree) -> String {
        let markup = tree.to_string(&WriteOptions {
            id_prefix: Some(format!("{}-", self.unique_id("svg"))),
            ..WriteOptions::default()
        });
        let start = markup.find('>').map_or(0, |index| index + 1);
        let end = markup.rfind("</svg>").unwrap_or(start).max(start);
        markup[start..end].to_owned()
    }

    fn paint_attributes(
        &mut self,
        fill: Option<&Paint>,
        stroke: Option<&Stroke>,
        bounds: Bounds,
    ) -> String {
        let fill = match fill {
            Some(fill) => self.fill_attributes(fill, bounds),
            None => r#"fill="none""#.to_owned(),
        };
        match stroke {
            Some(stroke) if stroke.width != 0.0 => format!("{fill} {}", stroke_attributes(stroke)),
            _ => fill,
        }
    }

    fn fill_attributes(&mut self, paint: &Paint, bounds: Bounds) -> String {
        match paint {
            Paint::Solid { color } => color_attributes("fill", *color),
            Paint::LinearGradient { angle, stops } => {
                let id = self.unique_id("gradient");
                let ((x1, y1), (x2, y2)) = linear_gradient_line(*angle, bounds);
                let stops = stop_elements(stops);
                let _ = write!(
                    self.definitions,
                    r#"<linearGradient id="{id}" gradientUnits="userSpaceOnUse" x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}">{stops}</linearGradient>"#
                );
                format!(r#"fill="url(#{id})""#)
            }
            Paint::RadialGradient {
                center,
                radius,
                stops,
            } => {
                let id = self.unique_id("gradient");
                let transform = matrix(Affine {
                    a: radius * bounds.width,
                    b: 0.0,
                    c: 0.0,
                    d: radius * bounds.height,
                    e: bounds.x + center[0] * bounds.width,
                    f: bounds.y + center[1] * bounds.height,
                });
                let stops = stop_elements(stops);
                let _ = write!(
                    self.definitions,
                    r#"<radialGradient id="{id}" gradientUnits="userSpaceOnUse" cx="0" cy="0" r="1" gradientTransform="{transform}">{stops}</radialGradient>"#
                );
                format!(r#"fill="url(#{id})""#)
            }
        }
    }
}

fn push(output: &mut String, element: String) {
    output.push_str(&element);
}

/// Masks need an explicit region. This one leaves room for strokes around the mask's box.
fn mask_region(mask: &SceneNode) -> Bounds {
    let bounds = mask.bounds.transformed(mask.transform);
    let margin = bounds.width.max(bounds.height);
    Bounds {
        x: bounds.x - margin,
        y: bounds.y - margin,
        width: bounds.width + margin * 2.0,
        height: bounds.height + margin * 2.0,
    }
}

fn matrix(transform: Affine) -> String {
    let Affine { a, b, c, d, e, f } = transform;
    format!("matrix({a} {b} {c} {d} {e} {f})")
}

fn transform_attribute(transform: Affine) -> String {
    if transform == Affine::IDENTITY {
        String::new()
    } else {
        format!(r#" transform="{}""#, matrix(transform))
    }
}

fn opacity_attribute(opacity: f64) -> String {
    if opacity >= 1.0 {
        String::new()
    } else {
        format!(r#" opacity="{opacity}""#)
    }
}

fn hex(color: Color) -> String {
    let Color {
        red, green, blue, ..
    } = color;
    format!("#{red:02X}{green:02X}{blue:02X}")
}

fn color_attributes(attribute: &str, color: Color) -> String {
    let hex = hex(color);
    if color.alpha == u8::MAX {
        format!(r#"{attribute}="{hex}""#)
    } else {
        let opacity = f64::from(color.alpha) / 255.0;
        format!(r#"{attribute}="{hex}" {attribute}-opacity="{opacity}""#)
    }
}

fn stop_elements(stops: &[ColorStop]) -> String {
    stops
        .iter()
        .map(|stop| {
            let color = hex(stop.color);
            let opacity = f64::from(stop.color.alpha) / 255.0;
            format!(
                r#"<stop offset="{}" stop-color="{color}" stop-opacity="{opacity}"/>"#,
                stop.offset
            )
        })
        .collect()
}

fn stroke_attributes(stroke: &Stroke) -> String {
    let hex = hex(stroke.color);
    let opacity = stroke.opacity * f64::from(stroke.color.alpha) / 255.0;
    let cap = match stroke.cap {
        LineCap::Butt => "butt",
        LineCap::Round => "round",
        LineCap::Square => "square",
    };
    let join = match stroke.join {
        LineJoin::Miter => "miter",
        LineJoin::Round => "round",
        LineJoin::Bevel => "bevel",
    };
    let mut attributes = format!(
        r#"stroke="{hex}" stroke-opacity="{opacity}" stroke-width="{}" stroke-linecap="{cap}" stroke-linejoin="{join}""#,
        stroke.width
    );
    if !stroke.dash.is_empty() {
        let dash: Vec<String> = stroke.dash.iter().map(ToString::to_string).collect();
        let _ = write!(attributes, r#" stroke-dasharray="{}""#, dash.join(" "));
    }
    attributes
}

fn path_data(path: &tiny_skia::Path) -> String {
    let mut data = String::new();
    for segment in path.segments() {
        // Writing to a string cannot fail.
        let _ = match segment {
            PathSegment::MoveTo(point) => write!(data, "M{} {}", point.x, point.y),
            PathSegment::LineTo(point) => write!(data, "L{} {}", point.x, point.y),
            PathSegment::QuadTo(control, point) => {
                write!(data, "Q{} {} {} {}", control.x, control.y, point.x, point.y)
            }
            PathSegment::CubicTo(first, second, point) => write!(
                data,
                "C{} {} {} {} {} {}",
                first.x, first.y, second.x, second.y, point.x, point.y
            ),
            PathSegment::Close => write!(data, "Z"),
        };
    }
    data
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use serde_json::json;
    use tiny_skia::{Pixmap, Transform};

    use super::*;
    use crate::assets::Assets;
    use crate::document::parse_frame;
    use crate::renderer;
    use crate::scene::{Resources, build};
    use crate::text::FontLibrary;
    use crate::tokens::Tokens;

    /// Every kind of node, on a transparent frame so alpha differences count too.
    fn scene_with_everything(assets_directory: &Path) -> Scene {
        fs::copy(
            "tests/fixtures/Tuffy.ttf",
            assets_directory.join("Tuffy.ttf"),
        )
        .unwrap();
        let image =
            image::RgbaImage::from_fn(8, 4, |x, _| image::Rgba([(x * 36) as u8, 80, 200, 255]));
        image.save(assets_directory.join("tile.png")).unwrap();
        fs::write(
            assets_directory.join("mark.svg"),
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40">
                <defs><linearGradient id="g"><stop offset="0" stop-color="#F00"/>
                <stop offset="1" stop-color="#00F"/></linearGradient></defs>
                <circle cx="20" cy="20" r="18" fill="url(#g)"/></svg>"##,
        )
        .unwrap();
        let frame = json!({
            "version": 1, "id": "everything", "width": 400, "height": 300,
            "background": {
                "type": "linear-gradient", "angle": 135,
                "stops": [{ "offset": 0, "color": "#101014" }, { "offset": 1, "color": "#34346A" }]
            },
            "children": [
                { "id": "rounded", "type": "rect", "x": 20, "y": 20, "width": 100, "height": 60,
                  "cornerRadius": 12, "rotation": 8, "opacity": 0.8,
                  "fill": { "type": "radial-gradient", "center": [0.5, 0.5], "radius": 0.7, "stops": [
                      { "offset": 0, "color": "#FF2D9B" }, { "offset": 1, "color": "#914DFF" }] },
                  "stroke": { "color": "#FFFFFF", "width": 3, "dash": [6, 3] } },
                { "id": "dot", "type": "ellipse", "x": 150, "y": 20, "width": 60, "height": 40,
                  "fill": { "color": "#E8FF5780" } },
                { "id": "rule", "type": "line", "x": 20, "y": 110, "width": 180, "height": 0,
                  "stroke": { "color": "#FFFFFF", "width": 4, "cap": "round" } },
                { "id": "title", "type": "text", "text": "Hi <there>", "x": 20, "y": 130,
                  "font": { "file": "Tuffy.ttf", "size": 40 }, "fill": { "color": "#FFFFFF" } },
                { "id": "fill-image", "type": "image", "src": "tile.png",
                  "x": 230, "y": 20, "width": 60, "height": 60, "fit": "cover" },
                { "id": "fit-image", "type": "image", "src": "tile.png",
                  "x": 300, "y": 20, "width": 60, "height": 60, "fit": "contain" },
                { "id": "mark", "type": "svg", "src": "mark.svg",
                  "x": 230, "y": 100, "width": 80, "height": 40 },
                { "id": "hidden", "type": "rect", "width": 400, "height": 300, "visible": false,
                  "fill": { "color": "#FF0000" } },
                { "id": "clipped", "type": "group", "x": 20, "y": 190, "width": 120, "height": 80,
                  "clip": true, "opacity": 0.9, "children": [
                      { "id": "overhang", "type": "rect", "x": -40, "y": -40, "width": 120,
                        "height": 90, "fill": { "color": "#0D99FF" } } ] },
                { "id": "masked", "type": "group", "x": 170, "y": 190,
                  "mask": { "id": "shape", "type": "ellipse", "width": 100, "height": 80,
                            "fill": { "color": "#20408080" } },
                  "children": [
                      { "id": "under", "type": "rect", "width": 120, "height": 80,
                        "fill": { "color": "#E8FF57" } } ] },
            ],
        });
        let resources = Resources {
            tokens: Tokens::parse("{}").unwrap(),
            assets: Assets::new(assets_directory.to_owned()),
            fonts: FontLibrary::default(),
        };
        build(&parse_frame(&frame.to_string()).unwrap(), &resources).unwrap()
    }

    fn rendered_svg(markup: &str, width: u32, height: u32) -> Pixmap {
        let tree = Tree::from_str(markup, &usvg::Options::default()).unwrap();
        let mut canvas = Pixmap::new(width, height).unwrap();
        resvg::render(&tree, Transform::identity(), &mut canvas.as_mut());
        canvas
    }

    /// Share of pixels that are visibly off. An average would hide a whole missing shape.
    fn share_of_differing_pixels(left: &Pixmap, right: &Pixmap) -> f64 {
        const VISIBLE_DIFFERENCE: u8 = 24;
        let differing = left
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .zip(right.data().as_chunks::<4>().0)
            .filter(|(left, right)| {
                left.iter()
                    .zip(right.iter())
                    .any(|(left, right)| left.abs_diff(*right) > VISIBLE_DIFFERENCE)
            })
            .count();
        differing as f64 / (left.width() * left.height()) as f64
    }

    #[test]
    fn the_export_draws_the_same_picture_as_the_renderer() {
        let directory = tempfile::tempdir().unwrap();
        let scene = scene_with_everything(directory.path());

        let expected = renderer::render(&scene, 1.0).unwrap();
        let exported = rendered_svg(&to_svg(&scene), expected.width(), expected.height());

        let difference = share_of_differing_pixels(&expected, &exported);
        assert!(difference < 0.005, "{difference} of the pixels differ");
    }

    #[test]
    fn layers_keep_their_names_and_hidden_ones_stay_out() {
        let directory = tempfile::tempdir().unwrap();
        let markup = to_svg(&scene_with_everything(directory.path()));

        assert!(markup.contains(r#"id="rounded""#));
        assert!(markup.contains(r#"id="clipped""#));
        assert!(!markup.contains(r#"id="hidden""#));
    }

    #[test]
    fn the_export_needs_no_outside_files() {
        let directory = tempfile::tempdir().unwrap();
        let markup = to_svg(&scene_with_everything(directory.path()));

        assert!(markup.contains("data:image/png;base64,"));
        assert!(!markup.contains("tile.png"));
        assert!(!markup.contains("Tuffy"));
    }

    #[test]
    fn layer_ids_are_escaped() {
        assert_eq!(escape_xml(r#"a<"b">&"#), "a&lt;&quot;b&quot;&gt;&amp;");
    }

    #[test]
    fn two_embedded_svgs_cannot_share_ids() {
        let directory = tempfile::tempdir().unwrap();
        let scene = scene_with_everything(directory.path());
        let SceneKind::Svg { tree, .. } = &scene.nodes[6].kind else {
            panic!("the seventh node is the svg");
        };
        let mut writer = SvgWriter::default();

        let first = writer.embedded_svg_content(tree);
        let second = writer.embedded_svg_content(tree);

        assert_ne!(first, second);
    }
}
