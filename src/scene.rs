//! The resolved, renderer-independent scene: tokens are values and geometry is computed.

use std::path::PathBuf;

use serde::Serialize;
use usvg::Tree;

use crate::assets::Assets;
use crate::diagnostics::{Diagnostic, Failure};
use crate::document::{
    self, CommonProperties, Frame, ImageFit, LineCap, LineJoin, Node, NodeKind, NumberValue,
};
use crate::text::{FontLibrary, TextRequest};
use crate::tokens::{Color, Paint, ResolvedFont, Tokens};

/// Maps `(x, y)` to `(a·x + c·y + e, b·x + d·y + f)`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Affine {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Affine {
    pub const IDENTITY: Self = Self::scale(1.0, 1.0);

    pub const fn translate(x: f64, y: f64) -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: x,
            f: y,
        }
    }

    pub const fn scale(x: f64, y: f64) -> Self {
        Self {
            a: x,
            b: 0.0,
            c: 0.0,
            d: y,
            e: 0.0,
            f: 0.0,
        }
    }

    /// Clockwise on screen, because Y points down.
    pub fn rotate(degrees: f64) -> Self {
        let (sine, cosine) = degrees.to_radians().sin_cos();
        Self {
            a: cosine,
            b: sine,
            c: -sine,
            d: cosine,
            e: 0.0,
            f: 0.0,
        }
    }

    /// The transform that applies `self` first, then `next`.
    pub fn then(self, next: Self) -> Self {
        Self {
            a: next.a * self.a + next.c * self.b,
            b: next.b * self.a + next.d * self.b,
            c: next.a * self.c + next.c * self.d,
            d: next.b * self.c + next.d * self.d,
            e: next.a * self.e + next.c * self.f + next.e,
            f: next.b * self.e + next.d * self.f + next.f,
        }
    }

    pub fn apply(self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Bounds {
    pub const fn sized(width: f64, height: f64) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width,
            height,
        }
    }

    fn from_points(points: impl IntoIterator<Item = (f64, f64)>) -> Option<Self> {
        let (min_x, min_y, max_x, max_y) = points.into_iter().fold(
            (
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ),
            |(min_x, min_y, max_x, max_y), (x, y)| {
                (min_x.min(x), min_y.min(y), max_x.max(x), max_y.max(y))
            },
        );
        (min_x <= max_x).then_some(Self {
            x: min_x,
            y: min_y,
            width: max_x - min_x,
            height: max_y - min_y,
        })
    }

    fn corners(self) -> [(f64, f64); 4] {
        let (right, bottom) = (self.x + self.width, self.y + self.height);
        [
            (self.x, self.y),
            (right, self.y),
            (right, bottom),
            (self.x, bottom),
        ]
    }

    fn intersection(self, other: Self) -> Option<Self> {
        let left = self.x.max(other.x);
        let top = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        (left < right && top < bottom).then_some(Self {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        })
    }

    /// The axis-aligned box around this box after `transform`.
    pub fn transformed(self, transform: Affine) -> Self {
        Self::from_points(self.corners().map(|(x, y)| transform.apply(x, y)))
            .expect("four corners are never empty")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Stroke {
    pub color: Color,
    pub width: f64,
    pub opacity: f64,
    #[serde(skip)]
    pub cap: LineCap,
    #[serde(skip)]
    pub join: LineJoin,
    pub dash: Vec<f64>,
}

#[derive(Debug)]
pub enum SceneKind {
    Rect {
        corner_radius: f64,
        fill: Option<Paint>,
        stroke: Option<Stroke>,
    },
    Ellipse {
        fill: Option<Paint>,
        stroke: Option<Stroke>,
    },
    Line {
        stroke: Stroke,
    },
    Text {
        outlines: Option<tiny_skia::Path>,
        fill: Paint,
        font: ResolvedFont,
        font_source: String,
        line_count: usize,
    },
    Image {
        image: tiny_skia::Pixmap,
        source: PathBuf,
        fit: ImageFit,
    },
    Svg {
        tree: Tree,
        source: PathBuf,
        color: Option<Color>,
    },
    Group {
        children: Vec<SceneNode>,
        clip: bool,
        mask: Option<Box<SceneNode>>,
    },
}

#[derive(Debug)]
pub struct SceneNode {
    pub id: String,
    /// Node-local coordinates to parent coordinates.
    pub transform: Affine,
    /// The node's box in node-local coordinates.
    pub bounds: Bounds,
    pub opacity: f64,
    pub visible: bool,
    pub kind: SceneKind,
}

/// Where a node ends up. Bounds cover the node's box, not stroke width or antialiasing.
#[derive(Debug, Clone, Copy)]
pub struct NodeLayout {
    pub world: Affine,
    pub world_bounds: Bounds,
    /// `None` when nothing of the node is inside the frame and its ancestors' clips.
    pub clipped_bounds: Option<Bounds>,
}

impl NodeLayout {
    pub fn of(node: &SceneNode, parent: Affine, visible_area: Bounds) -> Self {
        let world = node.transform.then(parent);
        let world_bounds = node.bounds.transformed(world);
        Self {
            world,
            world_bounds,
            clipped_bounds: world_bounds.intersection(visible_area),
        }
    }

    pub fn overflows_clip(&self) -> bool {
        self.clipped_bounds != Some(self.world_bounds)
    }

    /// For a rotated clip this is the box around it, so it can overestimate what is visible.
    pub fn children_visible_area(&self, clip: bool, visible_area: Bounds) -> Bounds {
        if clip {
            self.clipped_bounds.unwrap_or(Bounds::sized(0.0, 0.0))
        } else {
            visible_area
        }
    }
}

#[derive(Debug)]
pub struct Scene {
    pub id: String,
    pub width: f64,
    pub height: f64,
    pub background: Option<Paint>,
    pub nodes: Vec<SceneNode>,
    pub warnings: Vec<Diagnostic>,
}

impl Scene {
    pub fn frame_area(&self) -> Bounds {
        Bounds::sized(self.width, self.height)
    }
}

/// Everything outside a frame document that resolving it can depend on.
#[derive(Debug)]
pub struct Resources {
    pub tokens: Tokens,
    pub assets: Assets,
    pub fonts: FontLibrary,
}

/// Resolves a parsed frame, reporting every problem rather than the first.
pub fn build(frame: &Frame, resources: &Resources) -> Result<Scene, Failure> {
    let tokens = &resources.tokens;
    let mut builder = SceneBuilder {
        tokens,
        assets: &resources.assets,
        fonts: &resources.fonts,
        diagnostics: Vec::new(),
        warnings: Vec::new(),
    };
    let header = &frame.header;
    let width = builder.positive_frame_dimension(&header.width, "width");
    let height = builder.positive_frame_dimension(&header.height, "height");
    let background = header
        .background
        .as_ref()
        .and_then(|fill| builder.record(tokens.paint(fill, "background"), None));
    let nodes = builder.build_nodes(&frame.children, "children");
    if !builder.diagnostics.is_empty() {
        return Err(Failure(builder.diagnostics));
    }
    let mut scene = Scene {
        id: header.id.clone(),
        width: width.expect("failures were reported above"),
        height: height.expect("failures were reported above"),
        background,
        nodes,
        warnings: builder.warnings,
    };
    let mut clip_warnings = Vec::new();
    warn_about_clipped_nodes(
        &scene.nodes,
        Affine::IDENTITY,
        scene.frame_area(),
        &mut clip_warnings,
    );
    scene.warnings.append(&mut clip_warnings);
    Ok(scene)
}

fn warn_about_clipped_nodes(
    nodes: &[SceneNode],
    parent: Affine,
    visible_area: Bounds,
    warnings: &mut Vec<Diagnostic>,
) {
    for node in nodes.iter().filter(|node| node.visible) {
        let layout = NodeLayout::of(node, parent, visible_area);
        let has_area = node.bounds.width > 0.0 && node.bounds.height > 0.0;
        if has_area && layout.clipped_bounds.is_none() {
            warnings.push(
                Diagnostic::new("element_clipped", "this node is entirely clipped")
                    .on_layer(node.id.clone()),
            );
        }
        if let SceneKind::Group { children, clip, .. } = &node.kind {
            let children_area = layout.children_visible_area(*clip, visible_area);
            warn_about_clipped_nodes(children, layout.world, children_area, warnings);
        }
    }
}

struct SceneBuilder<'a> {
    tokens: &'a Tokens,
    assets: &'a Assets,
    fonts: &'a FontLibrary,
    diagnostics: Vec<Diagnostic>,
    warnings: Vec<Diagnostic>,
}

impl SceneBuilder<'_> {
    fn record<T>(&mut self, result: Result<T, Diagnostic>, layer: Option<&str>) -> Option<T> {
        result
            .map_err(|mut diagnostic| {
                diagnostic.layer = layer.map(str::to_owned);
                self.diagnostics.push(diagnostic);
            })
            .ok()
    }

    fn positive_frame_dimension(&mut self, value: &NumberValue, name: &str) -> Option<f64> {
        let number = self.record(self.tokens.number(value, name), None)?;
        if number > 0.0 {
            return Some(number);
        }
        self.diagnostics
            .push(Diagnostic::new("invalid_value", format!("{name} must be positive")).at(name));
        None
    }

    fn build_nodes(&mut self, nodes: &[Node], path: &str) -> Vec<SceneNode> {
        nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| self.build_node(node, &format!("{path}[{index}]")))
            .collect()
    }

    fn number(&mut self, value: &NumberValue, path: &str, layer: &str) -> Option<f64> {
        self.record(self.tokens.number(value, path), Some(layer))
    }

    fn dimension(&mut self, value: &NumberValue, path: &str, layer: &str) -> Option<f64> {
        let number = self.number(value, path, layer)?;
        if number >= 0.0 {
            return Some(number);
        }
        self.invalid(path, layer, "must not be negative");
        None
    }

    fn invalid(&mut self, path: &str, layer: &str, message: &str) {
        self.diagnostics.push(
            Diagnostic::new("invalid_value", message)
                .at(path)
                .on_layer(layer),
        );
    }

    fn paint(
        &mut self,
        fill: Option<&document::Fill>,
        path: &str,
        layer: &str,
    ) -> Option<Option<Paint>> {
        match fill {
            None => Some(None),
            Some(fill) => self
                .record(self.tokens.paint(fill, path), Some(layer))
                .map(Some),
        }
    }

    fn stroke(&mut self, stroke: &document::Stroke, path: &str, layer: &str) -> Option<Stroke> {
        let color = self.record(
            self.tokens.color(&stroke.color, &format!("{path}.color")),
            Some(layer),
        );
        let width = self.dimension(&stroke.width, &format!("{path}.width"), layer);
        let opacity = stroke.opacity.unwrap_or(1.0);
        if !(0.0..=1.0).contains(&opacity) {
            self.invalid(&format!("{path}.opacity"), layer, "must be within [0, 1]");
        }
        let dash = stroke.dash.clone().unwrap_or_default();
        if dash.iter().any(|length| *length < 0.0)
            || (!dash.is_empty() && dash.iter().sum::<f64>() <= 0.0)
        {
            self.invalid(
                &format!("{path}.dash"),
                layer,
                "lengths must not be negative and must have a positive sum",
            );
        }
        Some(Stroke {
            color: color?,
            width: width?,
            opacity,
            cap: stroke.cap.unwrap_or_default(),
            join: stroke.join.unwrap_or_default(),
            dash,
        })
    }

    fn optional_stroke(
        &mut self,
        stroke: Option<&document::Stroke>,
        path: &str,
        layer: &str,
    ) -> Option<Option<Stroke>> {
        match stroke {
            None => Some(None),
            Some(stroke) => self.stroke(stroke, path, layer).map(Some),
        }
    }

    fn build_node(&mut self, node: &Node, path: &str) -> Option<SceneNode> {
        let layer = node.common.id.as_str();
        let (kind, bounds, has_explicit_box) = match &node.kind {
            NodeKind::Rect(rect) => {
                let width = self.dimension(&rect.width, &format!("{path}.width"), layer);
                let height = self.dimension(&rect.height, &format!("{path}.height"), layer);
                let corner_radius = match &rect.corner_radius {
                    Some(radius) => self.dimension(radius, &format!("{path}.cornerRadius"), layer),
                    None => Some(0.0),
                };
                let fill = self.paint(rect.fill.as_ref(), &format!("{path}.fill"), layer);
                let stroke =
                    self.optional_stroke(rect.stroke.as_ref(), &format!("{path}.stroke"), layer);
                let kind = SceneKind::Rect {
                    corner_radius: corner_radius?,
                    fill: fill?,
                    stroke: stroke?,
                };
                (kind, Bounds::sized(width?, height?), true)
            }
            NodeKind::Ellipse(ellipse) => {
                let width = self.dimension(&ellipse.width, &format!("{path}.width"), layer);
                let height = self.dimension(&ellipse.height, &format!("{path}.height"), layer);
                let fill = self.paint(ellipse.fill.as_ref(), &format!("{path}.fill"), layer);
                let stroke =
                    self.optional_stroke(ellipse.stroke.as_ref(), &format!("{path}.stroke"), layer);
                let kind = SceneKind::Ellipse {
                    fill: fill?,
                    stroke: stroke?,
                };
                (kind, Bounds::sized(width?, height?), true)
            }
            NodeKind::Line(line) => {
                let width = self.dimension(&line.width, &format!("{path}.width"), layer);
                let height = self.dimension(&line.height, &format!("{path}.height"), layer);
                let stroke = self.stroke(&line.stroke, &format!("{path}.stroke"), layer)?;
                (
                    SceneKind::Line { stroke },
                    Bounds::sized(width?, height?),
                    true,
                )
            }
            NodeKind::Group(group) => {
                let children = self.build_nodes(&group.children, &format!("{path}.children"));
                let mask = group
                    .mask
                    .as_deref()
                    .and_then(|mask| self.build_node(mask, &format!("{path}.mask")))
                    .map(Box::new);
                let width = group
                    .properties
                    .width
                    .as_ref()
                    .map(|value| self.dimension(value, &format!("{path}.width"), layer));
                let height = group
                    .properties
                    .height
                    .as_ref()
                    .map(|value| self.dimension(value, &format!("{path}.height"), layer));
                let explicit_size = match (width, height) {
                    (Some(width), Some(height)) => Some((width?, height?)),
                    (None, None) => None,
                    _ => {
                        self.invalid(path, layer, "set both width and height, or neither");
                        return None;
                    }
                };
                let clip = group.properties.clip.unwrap_or(false);
                if clip && explicit_size.is_none() {
                    self.invalid(
                        &format!("{path}.clip"),
                        layer,
                        "clipping needs an explicit width and height",
                    );
                }
                let bounds = match explicit_size {
                    Some((width, height)) => Bounds::sized(width, height),
                    None => derived_group_bounds(&children),
                };
                (
                    SceneKind::Group {
                        children,
                        clip,
                        mask,
                    },
                    bounds,
                    explicit_size.is_some(),
                )
            }
            NodeKind::Image(image) => {
                let width = self.dimension(&image.width, &format!("{path}.width"), layer);
                let height = self.dimension(&image.height, &format!("{path}.height"), layer);
                let loaded = self.assets.load_image(&image.src, &format!("{path}.src"));
                let (source, decoded) = self.record(loaded, Some(layer))?;
                let kind = SceneKind::Image {
                    image: decoded,
                    source,
                    fit: image.fit.unwrap_or_default(),
                };
                (kind, Bounds::sized(width?, height?), true)
            }
            NodeKind::Svg(svg) => {
                let width = self.dimension(&svg.width, &format!("{path}.width"), layer);
                let height = self.dimension(&svg.height, &format!("{path}.height"), layer);
                let color_path = format!("{path}.color");
                let color = match &svg.color {
                    Some(color) => {
                        Some(self.record(self.tokens.color(color, &color_path), Some(layer))?)
                    }
                    None => None,
                };
                let loaded = self
                    .assets
                    .load_svg(&svg.src, color, &format!("{path}.src"));
                let (source, tree) = self.record(loaded, Some(layer))?;
                let kind = SceneKind::Svg {
                    tree,
                    source,
                    color,
                };
                (kind, Bounds::sized(width?, height?), true)
            }
            NodeKind::Text(text) => {
                let wrap_width = match &text.width {
                    Some(value) => Some(self.dimension(value, &format!("{path}.width"), layer)?),
                    None => None,
                };
                let font_path = format!("{path}.font");
                let font = self.record(self.tokens.font(&text.font, &font_path), Some(layer));
                let fill = self.paint(Some(&text.fill), &format!("{path}.fill"), layer);
                let font = font?;
                let request = TextRequest {
                    text: &text.text,
                    font: &font,
                    wrap_width,
                    align: text.align.unwrap_or_default(),
                    path: &font_path,
                };
                let layout = self.fonts.layout(&request, self.assets);
                let layout = self.record(layout, Some(layer))?;
                self.warnings.extend(
                    layout
                        .warnings
                        .into_iter()
                        .map(|warning| warning.on_layer(layer)),
                );
                let kind = SceneKind::Text {
                    outlines: layout.outlines,
                    fill: fill??,
                    font,
                    font_source: layout.font_source,
                    line_count: layout.line_count,
                };
                (kind, Bounds::sized(layout.width, layout.height), true)
            }
        };
        self.finish_node(&node.common, kind, bounds, has_explicit_box, path)
    }

    fn finish_node(
        &mut self,
        common: &CommonProperties,
        kind: SceneKind,
        bounds: Bounds,
        has_explicit_box: bool,
        path: &str,
    ) -> Option<SceneNode> {
        let layer = common.id.as_str();
        let x = match &common.x {
            Some(value) => self.number(value, &format!("{path}.x"), layer)?,
            None => 0.0,
        };
        let y = match &common.y {
            Some(value) => self.number(value, &format!("{path}.y"), layer)?,
            None => 0.0,
        };
        let opacity = common.opacity.unwrap_or(1.0);
        if !(0.0..=1.0).contains(&opacity) {
            self.invalid(&format!("{path}.opacity"), layer, "must be within [0, 1]");
            return None;
        }
        let (scale_x, scale_y) = (common.scale_x.unwrap_or(1.0), common.scale_y.unwrap_or(1.0));
        if scale_x == 0.0 || scale_y == 0.0 {
            self.warnings.push(
                Diagnostic::new(
                    "degenerate_geometry",
                    "a zero scale makes this node invisible",
                )
                .at(path)
                .on_layer(layer),
            );
        }
        // A group sized by its children keeps its own origin unless an anchor is requested,
        // so a group at x=0 does not shift its children to the left edge.
        let anchor_point = match common.anchor {
            None if !has_explicit_box => (0.0, 0.0),
            anchor => bounds_point(bounds, anchor.unwrap_or([0.0, 0.0])),
        };
        let pivot_point = common
            .pivot
            .map_or(anchor_point, |pivot| bounds_point(bounds, pivot));
        Some(SceneNode {
            id: common.id.clone(),
            transform: node_transform(
                (x, y),
                anchor_point,
                pivot_point,
                common.rotation.unwrap_or(0.0),
                (scale_x, scale_y),
            ),
            bounds,
            opacity,
            visible: common.visible.unwrap_or(true),
            kind,
        })
    }
}

fn bounds_point(bounds: Bounds, normalized: [f64; 2]) -> (f64, f64) {
    (
        bounds.x + normalized[0] * bounds.width,
        bounds.y + normalized[1] * bounds.height,
    )
}

fn derived_group_bounds(children: &[SceneNode]) -> Bounds {
    Bounds::from_points(
        children
            .iter()
            .flat_map(|child| child.bounds.transformed(child.transform).corners()),
    )
    .unwrap_or(Bounds::sized(0.0, 0.0))
}

/// Puts the untransformed anchor at `position`, then scales and rotates around the pivot.
fn node_transform(
    position: (f64, f64),
    anchor: (f64, f64),
    pivot: (f64, f64),
    rotation: f64,
    scale: (f64, f64),
) -> Affine {
    Affine::translate(-pivot.0, -pivot.1)
        .then(Affine::scale(scale.0, scale.1))
        .then(Affine::rotate(rotation))
        .then(Affine::translate(pivot.0, pivot.1))
        .then(Affine::translate(
            position.0 - anchor.0,
            position.1 - anchor.1,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_near(actual: (f64, f64), expected: (f64, f64)) {
        let distance = (actual.0 - expected.0).hypot(actual.1 - expected.1);
        assert!(distance < 1e-9, "{actual:?} is not {expected:?}");
    }

    #[test]
    fn anchor_places_the_box_and_rotation_turns_clockwise_around_the_pivot() {
        // A 100×40 box centered at (500, 300), turned a quarter clockwise.
        let transform =
            node_transform((500.0, 300.0), (50.0, 20.0), (50.0, 20.0), 90.0, (2.0, 1.0));

        assert_near(transform.apply(50.0, 20.0), (500.0, 300.0));
        // The right-edge midpoint is 100 away after scaling, and now points down.
        assert_near(transform.apply(100.0, 20.0), (500.0, 400.0));
    }

    #[test]
    fn a_pivot_apart_from_the_anchor_moves_the_anchored_point() {
        let transform = node_transform((10.0, 10.0), (0.0, 0.0), (100.0, 0.0), 180.0, (1.0, 1.0));

        // The box's top-left swings around the pivot at x=110 to x=210.
        assert_near(transform.apply(0.0, 0.0), (210.0, 10.0));
        assert_near(transform.apply(100.0, 0.0), (110.0, 10.0));
    }

    #[test]
    fn nested_transforms_compose_from_parent_to_child() {
        let parent = node_transform((100.0, 0.0), (0.0, 0.0), (0.0, 0.0), 90.0, (1.0, 1.0));
        let child = node_transform((10.0, 0.0), (0.0, 0.0), (0.0, 0.0), 0.0, (1.0, 1.0));

        assert_near(child.then(parent).apply(0.0, 0.0), (100.0, 10.0));
    }
}
