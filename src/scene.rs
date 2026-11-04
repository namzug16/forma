//! The resolved, renderer-independent scene: tokens are values and geometry is computed.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use usvg::Tree;

use crate::assets::Assets;
use crate::diagnostics::{Diagnostic, Failure};
use crate::document::{
    self, CommonProperties, Frame, ImageFit, LineCap, LineJoin, Node, NodeKind, NumberValue,
    SizeValue,
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

/// A possibly rounded box's fill and stroke, drawn by rects and behind a group's children.
#[derive(Debug)]
pub struct BoxPaint {
    pub corner_radius: f64,
    pub fill: Option<Paint>,
    pub stroke: Option<Stroke>,
}

#[derive(Debug)]
pub enum SceneKind {
    Rect(BoxPaint),
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
        image: Arc<tiny_skia::Pixmap>,
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
        background: BoxPaint,
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
    pub sizing: Sizing,
    pub kind: SceneKind,
}

/// How each side of the box was authored: `fixed`, `hug`, `fill` or a percentage.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Sizing {
    pub width: String,
    pub height: String,
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
    let frame_box = ParentBox {
        layer: None,
        width,
        height,
    };
    let nodes = builder.build_nodes(&frame.children, "children", frame_box);
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

    fn build_nodes(&mut self, nodes: &[Node], path: &str, parent: ParentBox) -> Vec<SceneNode> {
        nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| self.build_node(node, &format!("{path}[{index}]"), parent))
            .collect()
    }

    /// Before `minWidth` and friends apply.
    fn size(
        &mut self,
        value: Option<&SizeValue>,
        axis: Axis,
        parent: ParentBox,
        path: &str,
        layer: &str,
    ) -> Option<Size> {
        let path = format!("{path}.{}", axis.name());
        let fraction = match value {
            None | Some(SizeValue::Hug) => return Some(Size::Hug),
            Some(SizeValue::Number(number)) => {
                return self.dimension(number, &path, layer).map(Size::Fixed);
            }
            Some(SizeValue::Fill) => 1.0,
            Some(SizeValue::Percent(percent)) => percent / 100.0,
        };
        if let Some(extent) = parent.extent(axis) {
            return Some(Size::Fixed(extent * fraction));
        }
        // The frame lacks a size only when its own invalid size was already reported.
        let parent_layer = parent.layer?;
        self.invalid(
            &path,
            layer,
            &format!(
                "needs a parent with a known {axis}, but \"{parent_layer}\" sizes itself to its children",
                axis = axis.name()
            ),
        );
        None
    }

    /// The box of a node that has no content to hug.
    fn fixed_box(
        &mut self,
        width: &SizeValue,
        height: &SizeValue,
        parent: ParentBox,
        limits: Limits,
        path: &str,
        layer: &str,
    ) -> Option<Bounds> {
        let mut fixed =
            |value, axis: Axis| match self.size(Some(value), axis, parent, path, layer)? {
                Size::Fixed(size) => Some(limits.clamp(axis, size)),
                Size::Hug => {
                    self.invalid(
                        &format!("{path}.{}", axis.name()),
                        layer,
                        "only groups and text can hug their content",
                    );
                    None
                }
            };
        let width = fixed(width, Axis::Width);
        let height = fixed(height, Axis::Height);
        Some(Bounds::sized(width?, height?))
    }

    fn limits(&mut self, common: &CommonProperties, path: &str) -> Option<Limits> {
        let layer = common.id.as_str();
        let mut limit = |value: &Option<NumberValue>, name: &str, unset: f64| match value {
            Some(value) => self.dimension(value, &format!("{path}.{name}"), layer),
            None => Some(unset),
        };
        let min_width = limit(&common.min_width, "minWidth", 0.0);
        let max_width = limit(&common.max_width, "maxWidth", f64::INFINITY);
        let min_height = limit(&common.min_height, "minHeight", 0.0);
        let max_height = limit(&common.max_height, "maxHeight", f64::INFINITY);
        let limits = Limits {
            width: (min_width?, max_width?),
            height: (min_height?, max_height?),
        };
        let mut valid = true;
        for (axis, (min, max)) in [(Axis::Width, limits.width), (Axis::Height, limits.height)] {
            if min > max {
                let name = axis.name();
                let capitalized = if axis == Axis::Width {
                    "Width"
                } else {
                    "Height"
                };
                self.invalid(
                    &format!("{path}.max{capitalized}"),
                    layer,
                    &format!("must not be less than min{capitalized}, so the {name} has a range"),
                );
                valid = false;
            }
        }
        valid.then_some(limits)
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

    fn box_paint(
        &mut self,
        corner_radius: Option<&NumberValue>,
        fill: Option<&document::Fill>,
        stroke: Option<&document::Stroke>,
        path: &str,
        layer: &str,
    ) -> Option<BoxPaint> {
        let corner_radius = match corner_radius {
            Some(radius) => self.dimension(radius, &format!("{path}.cornerRadius"), layer),
            None => Some(0.0),
        };
        let fill = self.paint(fill, &format!("{path}.fill"), layer);
        let stroke = self.optional_stroke(stroke, &format!("{path}.stroke"), layer);
        Some(BoxPaint {
            corner_radius: corner_radius?,
            fill: fill?,
            stroke: stroke?,
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

    fn build_node(&mut self, node: &Node, path: &str, parent: ParentBox) -> Option<SceneNode> {
        let layer = node.common.id.as_str();
        let limits = self.limits(&node.common, path)?;
        let (kind, bounds, has_explicit_box) = match &node.kind {
            NodeKind::Rect(rect) => {
                let bounds = self.fixed_box(&rect.width, &rect.height, parent, limits, path, layer);
                let paint = self.box_paint(
                    rect.corner_radius.as_ref(),
                    rect.fill.as_ref(),
                    rect.stroke.as_ref(),
                    path,
                    layer,
                );
                (SceneKind::Rect(paint?), bounds?, true)
            }
            NodeKind::Ellipse(ellipse) => {
                let bounds =
                    self.fixed_box(&ellipse.width, &ellipse.height, parent, limits, path, layer);
                let fill = self.paint(ellipse.fill.as_ref(), &format!("{path}.fill"), layer);
                let stroke =
                    self.optional_stroke(ellipse.stroke.as_ref(), &format!("{path}.stroke"), layer);
                let kind = SceneKind::Ellipse {
                    fill: fill?,
                    stroke: stroke?,
                };
                (kind, bounds?, true)
            }
            NodeKind::Line(line) => {
                let bounds = self.fixed_box(&line.width, &line.height, parent, limits, path, layer);
                let stroke = self.stroke(&line.stroke, &format!("{path}.stroke"), layer)?;
                (SceneKind::Line { stroke }, bounds?, true)
            }
            NodeKind::Group(group) => {
                let width = self.size(
                    group.properties.width.as_ref(),
                    Axis::Width,
                    parent,
                    path,
                    layer,
                )?;
                let height = self.size(
                    group.properties.height.as_ref(),
                    Axis::Height,
                    parent,
                    path,
                    layer,
                )?;
                let width = width.clamped(limits, Axis::Width);
                let height = height.clamped(limits, Axis::Height);
                let children_box = ParentBox {
                    layer: Some(layer),
                    width: width.fixed(),
                    height: height.fixed(),
                };
                let children =
                    self.build_nodes(&group.children, &format!("{path}.children"), children_box);
                let mask = group
                    .mask
                    .as_deref()
                    .and_then(|mask| self.build_node(mask, &format!("{path}.mask"), children_box))
                    .map(Box::new);
                let background = self.box_paint(
                    group.properties.corner_radius.as_ref(),
                    group.properties.fill.as_ref(),
                    group.properties.stroke.as_ref(),
                    path,
                    layer,
                );
                let derived = derived_group_bounds(&children);
                let (x, width) = match width {
                    Size::Fixed(width) => (0.0, width),
                    Size::Hug => (derived.x, limits.clamp(Axis::Width, derived.width)),
                };
                let (y, height) = match height {
                    Size::Fixed(height) => (0.0, height),
                    Size::Hug => (derived.y, limits.clamp(Axis::Height, derived.height)),
                };
                let has_explicit_box =
                    children_box.width.is_some() && children_box.height.is_some();
                (
                    SceneKind::Group {
                        children,
                        background: background?,
                        clip: group.properties.clip.unwrap_or(false),
                        mask,
                    },
                    Bounds {
                        x,
                        y,
                        width,
                        height,
                    },
                    has_explicit_box,
                )
            }
            NodeKind::Image(image) => {
                let bounds =
                    self.fixed_box(&image.width, &image.height, parent, limits, path, layer);
                let loaded = self.assets.load_image(&image.src, &format!("{path}.src"));
                let (source, decoded) = self.record(loaded, Some(layer))?;
                let kind = SceneKind::Image {
                    image: decoded,
                    source,
                    fit: image.fit.unwrap_or_default(),
                };
                (kind, bounds?, true)
            }
            NodeKind::Svg(svg) => {
                let bounds = self.fixed_box(&svg.width, &svg.height, parent, limits, path, layer);
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
                (kind, bounds?, true)
            }
            NodeKind::Text(text) => {
                let width = self.size(text.width.as_ref(), Axis::Width, parent, path, layer);
                let font_path = format!("{path}.font");
                let font = self.record(self.tokens.font(&text.font, &font_path), Some(layer));
                let fill = self.paint(Some(&text.fill), &format!("{path}.fill"), layer);
                let font = font?;
                let mut request = TextRequest {
                    text: &text.text,
                    font: &font,
                    wrap_width: width?.clamped(limits, Axis::Width).fixed(),
                    align: text.align.unwrap_or_default(),
                    path: &font_path,
                };
                let mut layout = self.fonts.layout(&request, self.assets);
                // Hugging text wraps at `maxWidth`, and widens to `minWidth` so alignment holds.
                if let Ok(natural) = &layout
                    && request.wrap_width.is_none()
                    && limits.clamp(Axis::Width, natural.width) != natural.width
                {
                    request.wrap_width = Some(limits.clamp(Axis::Width, natural.width));
                    layout = self.fonts.layout(&request, self.assets);
                }
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
                let height = limits.clamp(Axis::Height, layout.height);
                (kind, Bounds::sized(layout.width, height), true)
            }
        };
        let mut scene_node =
            self.finish_node(&node.common, kind, bounds, has_explicit_box, path)?;
        scene_node.sizing = authored_sizing(&node.kind);
        Some(scene_node)
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
            sizing: Sizing::default(),
            kind,
        })
    }
}

fn authored_sizing(kind: &NodeKind) -> Sizing {
    let (width, height) = match kind {
        NodeKind::Rect(rect) => (Some(&rect.width), Some(&rect.height)),
        NodeKind::Ellipse(ellipse) => (Some(&ellipse.width), Some(&ellipse.height)),
        NodeKind::Line(line) => (Some(&line.width), Some(&line.height)),
        NodeKind::Image(image) => (Some(&image.width), Some(&image.height)),
        NodeKind::Svg(svg) => (Some(&svg.width), Some(&svg.height)),
        NodeKind::Text(text) => (text.width.as_ref(), None),
        NodeKind::Group(group) => (
            group.properties.width.as_ref(),
            group.properties.height.as_ref(),
        ),
    };
    let label = |value: Option<&SizeValue>| match value {
        None | Some(SizeValue::Hug) => "hug".to_owned(),
        Some(SizeValue::Number(_)) => "fixed".to_owned(),
        Some(SizeValue::Fill) => "fill".to_owned(),
        Some(SizeValue::Percent(percent)) => format!("{percent}%"),
    };
    Sizing {
        width: label(width),
        height: label(height),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Axis {
    Width,
    Height,
}

impl Axis {
    fn name(self) -> &'static str {
        match self {
            Self::Width => "width",
            Self::Height => "height",
        }
    }
}

/// What `fill` and percentages measure against: `None` on an axis the parent sizes to its
/// children, since the children's sizes would then depend on each other.
#[derive(Debug, Clone, Copy)]
struct ParentBox<'a> {
    /// `None` for the frame.
    layer: Option<&'a str>,
    width: Option<f64>,
    height: Option<f64>,
}

impl ParentBox<'_> {
    fn extent(&self, axis: Axis) -> Option<f64> {
        match axis {
            Axis::Width => self.width,
            Axis::Height => self.height,
        }
    }
}

/// A size before content is measured.
#[derive(Debug, Clone, Copy)]
enum Size {
    Fixed(f64),
    Hug,
}

impl Size {
    fn fixed(self) -> Option<f64> {
        match self {
            Self::Fixed(size) => Some(size),
            Self::Hug => None,
        }
    }

    fn clamped(self, limits: Limits, axis: Axis) -> Self {
        match self {
            Self::Fixed(size) => Self::Fixed(limits.clamp(axis, size)),
            Self::Hug => Self::Hug,
        }
    }
}

/// `(min, max)` per axis, with `min <= max`.
#[derive(Debug, Clone, Copy)]
struct Limits {
    width: (f64, f64),
    height: (f64, f64),
}

impl Limits {
    fn clamp(self, axis: Axis, size: f64) -> f64 {
        let (min, max) = match axis {
            Axis::Width => self.width,
            Axis::Height => self.height,
        };
        size.clamp(min, max)
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
    use crate::document::parse_frame;

    /// Builds a 200×100 frame whose assets directory holds the Tuffy fixture font.
    fn build_frame(children: &str) -> Result<Scene, Failure> {
        let directory = tempfile::tempdir().unwrap();
        std::fs::copy(
            "tests/fixtures/Tuffy.ttf",
            directory.path().join("Tuffy.ttf"),
        )
        .unwrap();
        let resources = Resources {
            tokens: Tokens::parse(r#"{ "spacing": { "wide": 80 } }"#).unwrap(),
            assets: Assets::new(directory.path().to_owned()),
            fonts: FontLibrary::default(),
        };
        let frame = format!(
            r#"{{ "version": 1, "id": "f", "width": 200, "height": 100, "children": [{children}] }}"#
        );
        build(&parse_frame(&frame).unwrap(), &resources)
    }

    fn only_error(children: &str) -> Diagnostic {
        let mut diagnostics = build_frame(children).unwrap_err().0;
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        diagnostics.remove(0)
    }

    fn children(node: &SceneNode) -> &[SceneNode] {
        let SceneKind::Group { children, .. } = &node.kind else {
            panic!("{} is a group", node.id);
        };
        children
    }

    #[test]
    fn fill_and_percentages_measure_against_the_frame_and_explicit_groups() {
        let scene = build_frame(
            r#"{ "id": "backdrop", "type": "rect", "width": "fill", "height": "50%" },
               { "id": "panel", "type": "group", "width": "$spacing.wide", "height": 40,
                 "children": [
                   { "id": "bar", "type": "rect", "width": "25%", "height": "fill" } ] }"#,
        )
        .unwrap();

        assert_eq!(scene.nodes[0].bounds, Bounds::sized(200.0, 50.0));
        assert_eq!(
            children(&scene.nodes[1])[0].bounds,
            Bounds::sized(20.0, 40.0)
        );
        assert_eq!(scene.nodes[0].sizing.height, "50%");
    }

    #[test]
    fn filling_a_parent_that_hugs_its_children_names_both_layers() {
        let error = only_error(
            r#"{ "id": "card", "type": "group", "width": 100, "children": [
                 { "id": "bar", "type": "rect", "width": "fill", "height": "fill" } ] }"#,
        );

        assert_eq!(error.layer.as_deref(), Some("bar"));
        assert!(error.message.contains("known height"), "{}", error.message);
        assert!(error.message.contains("\"card\""), "{}", error.message);
    }

    #[test]
    fn only_groups_and_text_can_hug() {
        let error = only_error(r#"{ "id": "box", "type": "rect", "width": "hug", "height": 10 }"#);

        assert!(error.message.contains("hug"), "{}", error.message);
    }

    #[test]
    fn min_and_max_clamp_every_kind_of_size_and_must_form_a_range() {
        let scene = build_frame(
            r#"{ "id": "wide", "type": "rect", "width": "fill", "height": 10, "maxWidth": 120 },
               { "id": "padded", "type": "group", "minWidth": 50, "minHeight": 30, "children": [
                   { "id": "dot", "type": "rect", "x": 5, "y": 5, "width": 10, "height": 10 } ] }"#,
        )
        .unwrap();

        assert_eq!(scene.nodes[0].bounds.width, 120.0);
        assert_eq!(
            scene.nodes[1].bounds,
            Bounds {
                x: 5.0,
                y: 5.0,
                width: 50.0,
                height: 30.0
            }
        );

        let error = only_error(
            r#"{ "id": "box", "type": "rect", "width": 10, "height": 10,
                 "minWidth": 20, "maxWidth": 10 }"#,
        );
        assert_eq!(error.path.as_deref(), Some("children[0].maxWidth"));
    }

    #[test]
    fn an_unknown_size_keyword_is_rejected_while_parsing() {
        let frame = r#"{ "version": 1, "id": "f", "width": 10, "height": 10, "children": [
            { "id": "box", "type": "rect", "width": "auto", "height": 10 } ] }"#;
        let error = &parse_frame(frame).unwrap_err().0[0];

        assert_eq!(error.code, "invalid_value");
        assert_eq!(error.path.as_deref(), Some("children[0].width"));
    }

    #[test]
    fn hugging_text_wraps_at_its_max_width() {
        let text = |max_width: &str| {
            let scene = build_frame(&format!(
                r##"{{ "id": "headline", "type": "text", "text": "Design as code, not coordinates",
                       "font": {{ "file": "Tuffy.ttf", "size": 20 }}, "fill": {{ "color": "#000" }}
                       {max_width} }}"##
            ))
            .unwrap();
            let node = scene.nodes.into_iter().next().unwrap();
            let SceneKind::Text { line_count, .. } = node.kind else {
                panic!("headline is text");
            };
            (node.bounds.width, line_count)
        };

        let (natural_width, natural_lines) = text("");
        assert_eq!(natural_lines, 1);
        assert!(natural_width > 100.0);
        let (wrapped_width, wrapped_lines) = text(r#", "maxWidth": 100"#);
        assert_eq!(wrapped_width, 100.0);
        assert!(wrapped_lines > 1);
    }

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
