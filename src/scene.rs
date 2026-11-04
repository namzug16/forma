//! The resolved, renderer-independent scene: tokens are values and geometry is computed.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use usvg::Tree;

use crate::assets::Assets;
use crate::diagnostics::{Diagnostic, Failure};
use taffy::prelude::TaffyMaxContent as _;
use taffy::{NodeId, TaffyTree};

use crate::document::{
    self, CommonProperties, Direction, Frame, GroupNode, ImageFit, LineCap, LineJoin, Node,
    NodeKind, NumberValue, Padding, SizeValue,
};
use crate::layout::{self, AxisSize, ResolvedLayout, Sides};
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
    builder.reject_absolute_children(&frame.children, "children");
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
            .filter_map(|(index, node)| {
                self.build_node(
                    node,
                    &format!("{path}[{index}]"),
                    parent,
                    Imposed::default(),
                )
            })
            .collect()
    }

    /// Before `minWidth` and friends apply.
    fn size(
        &mut self,
        value: Option<&SizeValue>,
        axis: Axis,
        parent: ParentBox,
        imposed: Imposed,
        path: &str,
        layer: &str,
    ) -> Option<Size> {
        match imposed.extent(axis) {
            Some(size) => return Some(Size::Fixed(size)),
            None if imposed.measuring => return Some(Size::Hug),
            None => {}
        }
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
    #[allow(clippy::too_many_arguments)]
    fn fixed_box(
        &mut self,
        width: &SizeValue,
        height: &SizeValue,
        parent: ParentBox,
        imposed: Imposed,
        limits: Limits,
        path: &str,
        layer: &str,
    ) -> Option<Bounds> {
        let mut fixed =
            |value, axis: Axis| match self.size(Some(value), axis, parent, imposed, path, layer)? {
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

    fn reject_absolute_children(&mut self, children: &[Node], path: &str) {
        for (index, child) in children.iter().enumerate() {
            if child.common.position.is_some() {
                self.invalid(
                    &format!("{path}[{index}].position"),
                    &child.common.id,
                    "only children of a layout group can leave its flow; place this layer with x and y",
                );
            }
        }
    }

    fn resolved_layout(
        &mut self,
        layout: &document::Layout,
        path: &str,
        layer: &str,
    ) -> Option<ResolvedLayout> {
        let path = format!("{path}.layout");
        let gap = match &layout.gap {
            Some(gap) => self.dimension(gap, &format!("{path}.gap"), layer),
            None => Some(0.0),
        };
        let padding_path = format!("{path}.padding");
        let mut side = |value: &NumberValue| self.dimension(value, &padding_path, layer);
        let padding = match &layout.padding {
            None => Some(Sides::default()),
            Some(Padding::All(all)) => side(all).map(|all| Sides {
                top: all,
                right: all,
                bottom: all,
                left: all,
            }),
            Some(Padding::Axes([vertical, horizontal])) => {
                let (vertical, horizontal) = (side(vertical), side(horizontal));
                Some(Sides {
                    top: vertical?,
                    right: horizontal?,
                    bottom: vertical?,
                    left: horizontal?,
                })
            }
            Some(Padding::Sides([top, right, bottom, left])) => {
                let sides = [side(top), side(right), side(bottom), side(left)];
                let [top, right, bottom, left] = sides;
                Some(Sides {
                    top: top?,
                    right: right?,
                    bottom: bottom?,
                    left: left?,
                })
            }
        };
        Some(ResolvedLayout {
            direction: layout.direction.unwrap_or_default(),
            gap: gap?,
            padding: padding?,
            align: layout.align.unwrap_or_default(),
            cross_align: layout.cross_align.unwrap_or_default(),
        })
    }

    /// Builds a layout group's children where its flexbox puts them, and returns the group's
    /// size. The outermost layout group computes the tree; nested ones read their part of it.
    #[allow(clippy::too_many_arguments)]
    fn build_layout_children<'d>(
        &mut self,
        node: &'d Node,
        group: &'d GroupNode,
        layout: &ResolvedLayout,
        (width, height): (Size, Size),
        limits: Limits,
        imposed: Imposed<'_, 'd>,
        path: &str,
    ) -> Option<(Vec<SceneNode>, f64, f64)> {
        let layer = node.common.id.as_str();
        let computed;
        let (tree, group_id) = match imposed.layout {
            Some(placed) => placed,
            None => {
                computed = self.compute_layout(node, path, (width, height), limits);
                (&computed.0, computed.1)
            }
        };
        let group_layout = tree.layout(group_id).expect("the group is in its own tree");
        let (width, height) = (
            f64::from(group_layout.size.width),
            f64::from(group_layout.size.height),
        );
        let padding = layout.padding;
        let content_box = ParentBox {
            layer: Some(layer),
            width: Some((width - padding.left - padding.right).max(0.0)),
            height: Some((height - padding.top - padding.bottom).max(0.0)),
        };
        let full_box = ParentBox {
            layer: Some(layer),
            width: Some(width),
            height: Some(height),
        };
        let mut flow_ids = tree
            .children(group_id)
            .expect("the group is in its own tree")
            .into_iter();
        let (mut left, mut top, mut right, mut bottom) = (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        );
        let mut children = Vec::new();
        for (index, child) in group.children.iter().enumerate() {
            let child_path = format!("{path}.children[{index}]");
            if !is_in_flow(child) {
                children.extend(self.build_node(child, &child_path, full_box, Imposed::default()));
                continue;
            }
            let child_id = flow_ids
                .next()
                .expect("the tree has a node for every child in the flow");
            self.check_flow_child(child, &child_path, group, layout.direction, layer);
            let placed = tree.layout(child_id).expect("children are in the tree");
            let (x, y) = (f64::from(placed.location.x), f64::from(placed.location.y));
            let (child_width, child_height) =
                (f64::from(placed.size.width), f64::from(placed.size.height));
            (left, top) = (left.min(x), top.min(y));
            (right, bottom) = (right.max(x + child_width), bottom.max(y + child_height));
            let child_imposed = Imposed {
                width: Some(child_width),
                height: Some(child_height),
                position: Some((x, y)),
                layout: layout_of(child).is_some().then_some((tree, child_id)),
                measuring: false,
            };
            children.extend(self.build_node(child, &child_path, content_box, child_imposed));
        }
        let overflow = |start_gap: f64, end_gap: f64| start_gap.max(0.0) + end_gap.max(0.0);
        let horizontal = overflow(padding.left - left, right - (width - padding.right));
        let vertical = overflow(padding.top - top, bottom - (height - padding.bottom));
        // taffy works in f32, so ignore overflow below its precision.
        if horizontal > 0.01 || vertical > 0.01 {
            let rounded = |pixels: f64| (pixels * 100.0).round() / 100.0;
            self.warnings.push(
                Diagnostic::new(
                    "layout_overflow",
                    format!(
                        "children overflow this group by {}px horizontally and {}px vertically",
                        rounded(horizontal),
                        rounded(vertical)
                    ),
                )
                .at(path)
                .on_layer(layer),
            );
        }
        Some((children, width, height))
    }

    /// Errors that the layout would otherwise resolve silently.
    fn check_flow_child(
        &mut self,
        child: &Node,
        path: &str,
        parent: &GroupNode,
        direction: Direction,
        parent_layer: &str,
    ) {
        let layer = child.common.id.as_str();
        let placed = [
            ("x", child.common.x.is_some()),
            ("y", child.common.y.is_some()),
            ("anchor", child.common.anchor.is_some()),
        ];
        for (property, is_set) in placed {
            if is_set {
                self.invalid(
                    &format!("{path}.{property}"),
                    layer,
                    &format!(
                        "the layout of \"{parent_layer}\" places this layer; remove `{property}`, or set \"position\": \"absolute\" to place it yourself"
                    ),
                );
            }
        }
        let (child_width, child_height) = authored_sizes(&child.kind);
        let parent_sizes = [
            parent.properties.width.as_ref(),
            parent.properties.height.as_ref(),
        ];
        let main_axis = match direction {
            Direction::Row => Axis::Width,
            Direction::Column => Axis::Height,
        };
        for ((axis, size), parent_size) in
            [(Axis::Width, child_width), (Axis::Height, child_height)]
                .into_iter()
                .zip(parent_sizes)
        {
            let parent_hugs = matches!(parent_size, None | Some(SizeValue::Hug));
            // Filling across a hugging layout stretches to the largest sibling, as in CSS.
            let is_circular = match size {
                Some(SizeValue::Fill) => axis == main_axis,
                Some(SizeValue::Percent(_)) => true,
                _ => false,
            };
            if parent_hugs && is_circular {
                self.invalid(
                    &format!("{path}.{}", axis.name()),
                    layer,
                    &format!(
                        "needs a parent with a known {axis}, but \"{parent_layer}\" sizes itself to its children",
                        axis = axis.name()
                    ),
                );
            }
        }
    }

    /// Lays out `node`, a layout group, with every nested layout group in one taffy tree.
    /// Problems are reported later, when each node is built, so this stays quiet.
    fn compute_layout<'d>(
        &mut self,
        node: &'d Node,
        path: &str,
        (width, height): (Size, Size),
        limits: Limits,
    ) -> (TaffyTree<Leaf<'d>>, NodeId) {
        let checkpoint = (self.diagnostics.len(), self.warnings.len());
        let mut tree = TaffyTree::new();
        tree.disable_rounding();
        let as_axis = |size: Size| match size {
            Size::Fixed(length) => AxisSize::Length(length),
            Size::Hug => AxisSize::Hug,
        };
        let root = self.add_to_layout_tree(
            &mut tree,
            node,
            path,
            None,
            Some((as_axis(width), as_axis(height), limits)),
        );
        tree.compute_layout_with_measure(
            root,
            taffy::Size::MAX_CONTENT,
            |input, _, leaf, style| {
                taffy::compute_leaf_layout(
                    input,
                    style,
                    |_, _| 0.0,
                    |known, _| match leaf.as_deref() {
                        Some(leaf) => self.measure(leaf, known),
                        None => taffy::Size::ZERO,
                    },
                )
            },
        )
        .expect("every node was just added to the tree");
        self.diagnostics.truncate(checkpoint.0);
        self.warnings.truncate(checkpoint.1);
        (tree, root)
    }

    /// `own` replaces the node's authored sizes, for the outermost group whose size is known.
    fn add_to_layout_tree<'d>(
        &mut self,
        tree: &mut TaffyTree<Leaf<'d>>,
        node: &'d Node,
        path: &str,
        parent_direction: Option<Direction>,
        own: Option<(AxisSize, AxisSize, Limits)>,
    ) -> NodeId {
        let layer = node.common.id.as_str();
        let (width, height, limits) = own.unwrap_or_else(|| {
            let (width, height) = authored_sizes(&node.kind);
            let limits = self.limits(&node.common, path).unwrap_or(Limits::NONE);
            (
                self.axis_size(width, &format!("{path}.width"), layer),
                self.axis_size(height, &format!("{path}.height"), layer),
                limits,
            )
        });
        let style =
            layout::item_style(width, height, limits.width, limits.height, parent_direction);
        let added = match (&node.kind, layout_of(node)) {
            (NodeKind::Group(group), Some(authored)) => {
                let layout =
                    self.resolved_layout(authored, path, layer)
                        .unwrap_or(ResolvedLayout {
                            direction: authored.direction.unwrap_or_default(),
                            gap: 0.0,
                            padding: Sides::default(),
                            align: authored.align.unwrap_or_default(),
                            cross_align: authored.cross_align.unwrap_or_default(),
                        });
                let children: Vec<NodeId> = group
                    .children
                    .iter()
                    .enumerate()
                    .filter(|(_, child)| is_in_flow(child))
                    .map(|(index, child)| {
                        self.add_to_layout_tree(
                            tree,
                            child,
                            &format!("{path}.children[{index}]"),
                            Some(layout.direction),
                            None,
                        )
                    })
                    .collect();
                tree.new_with_children(layout::container_style(style, &layout), &children)
            }
            (NodeKind::Text(_) | NodeKind::Group(_), None) => tree.new_leaf_with_context(
                style,
                Leaf {
                    node,
                    path: path.to_owned(),
                },
            ),
            _ => tree.new_leaf(style),
        };
        added.expect("taffy only fails to add nodes for unknown parents")
    }

    fn axis_size(&mut self, value: Option<&SizeValue>, path: &str, layer: &str) -> AxisSize {
        match value {
            None | Some(SizeValue::Hug) => AxisSize::Hug,
            Some(SizeValue::Fill) => AxisSize::Fill,
            Some(SizeValue::Percent(percent)) => AxisSize::Percent(*percent),
            Some(SizeValue::Number(number)) => self
                .dimension(number, path, layer)
                .map_or(AxisSize::Hug, AxisSize::Length),
        }
    }

    /// The size of text or a group without layout, given the sides taffy already knows.
    // shortcut: builds the node to measure it and throws the result away, cache measurements if
    // big frames get slow
    fn measure(&mut self, leaf: &Leaf, known: taffy::Size<Option<f32>>) -> taffy::Size<f32> {
        let checkpoint = (self.diagnostics.len(), self.warnings.len());
        let imposed = Imposed {
            width: known.width.map(f64::from),
            height: known.height.map(f64::from),
            measuring: true,
            ..Imposed::default()
        };
        let unknown_parent = ParentBox {
            layer: None,
            width: None,
            height: None,
        };
        let built = self.build_node(leaf.node, &leaf.path, unknown_parent, imposed);
        self.diagnostics.truncate(checkpoint.0);
        self.warnings.truncate(checkpoint.1);
        built.map_or(taffy::Size::ZERO, |built| taffy::Size {
            width: layout::at_least(built.bounds.width),
            height: layout::at_least(built.bounds.height),
        })
    }

    fn build_node<'d>(
        &mut self,
        node: &'d Node,
        path: &str,
        parent: ParentBox,
        imposed: Imposed<'_, 'd>,
    ) -> Option<SceneNode> {
        let layer = node.common.id.as_str();
        let limits = self.limits(&node.common, path)?;
        let (kind, bounds, has_explicit_box) = match &node.kind {
            NodeKind::Rect(rect) => {
                let bounds = self.fixed_box(
                    &rect.width,
                    &rect.height,
                    parent,
                    imposed,
                    limits,
                    path,
                    layer,
                );
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
                let bounds = self.fixed_box(
                    &ellipse.width,
                    &ellipse.height,
                    parent,
                    imposed,
                    limits,
                    path,
                    layer,
                );
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
                let bounds = self.fixed_box(
                    &line.width,
                    &line.height,
                    parent,
                    imposed,
                    limits,
                    path,
                    layer,
                );
                let stroke = self.stroke(&line.stroke, &format!("{path}.stroke"), layer)?;
                (SceneKind::Line { stroke }, bounds?, true)
            }
            NodeKind::Group(group) => {
                let width = self.size(
                    group.properties.width.as_ref(),
                    Axis::Width,
                    parent,
                    imposed,
                    path,
                    layer,
                )?;
                let height = self.size(
                    group.properties.height.as_ref(),
                    Axis::Height,
                    parent,
                    imposed,
                    path,
                    layer,
                )?;
                let width = width.clamped(limits, Axis::Width);
                let height = height.clamped(limits, Axis::Height);
                let (children, bounds, children_box) = match &group.properties.layout {
                    Some(authored) => {
                        let layout = self.resolved_layout(authored, path, layer)?;
                        let (children, width, height) = self.build_layout_children(
                            node,
                            group,
                            &layout,
                            (width, height),
                            limits,
                            imposed,
                            path,
                        )?;
                        let children_box = ParentBox {
                            layer: Some(layer),
                            width: Some(width),
                            height: Some(height),
                        };
                        (children, Bounds::sized(width, height), children_box)
                    }
                    None => {
                        let children_box = ParentBox {
                            layer: Some(layer),
                            width: width.fixed(),
                            height: height.fixed(),
                        };
                        let children_path = format!("{path}.children");
                        self.reject_absolute_children(&group.children, &children_path);
                        let children =
                            self.build_nodes(&group.children, &children_path, children_box);
                        let derived = derived_group_bounds(&children);
                        let (x, width) = match width {
                            Size::Fixed(width) => (0.0, width),
                            Size::Hug => (derived.x, limits.clamp(Axis::Width, derived.width)),
                        };
                        let (y, height) = match height {
                            Size::Fixed(height) => (0.0, height),
                            Size::Hug => (derived.y, limits.clamp(Axis::Height, derived.height)),
                        };
                        let bounds = Bounds {
                            x,
                            y,
                            width,
                            height,
                        };
                        (children, bounds, children_box)
                    }
                };
                let mask = group
                    .mask
                    .as_deref()
                    .and_then(|mask| {
                        self.build_node(
                            mask,
                            &format!("{path}.mask"),
                            children_box,
                            Imposed::default(),
                        )
                    })
                    .map(Box::new);
                let background = self.box_paint(
                    group.properties.corner_radius.as_ref(),
                    group.properties.fill.as_ref(),
                    group.properties.stroke.as_ref(),
                    path,
                    layer,
                );
                let has_explicit_box =
                    children_box.width.is_some() && children_box.height.is_some();
                (
                    SceneKind::Group {
                        children,
                        background: background?,
                        clip: group.properties.clip.unwrap_or(false),
                        mask,
                    },
                    bounds,
                    has_explicit_box,
                )
            }
            NodeKind::Image(image) => {
                let bounds = self.fixed_box(
                    &image.width,
                    &image.height,
                    parent,
                    imposed,
                    limits,
                    path,
                    layer,
                );
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
                let bounds = self.fixed_box(
                    &svg.width,
                    &svg.height,
                    parent,
                    imposed,
                    limits,
                    path,
                    layer,
                );
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
                let width = self.size(
                    text.width.as_ref(),
                    Axis::Width,
                    parent,
                    imposed,
                    path,
                    layer,
                );
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
                let height = imposed
                    .height
                    .unwrap_or_else(|| limits.clamp(Axis::Height, layout.height));
                (kind, Bounds::sized(layout.width, height), true)
            }
        };
        let mut scene_node =
            self.finish_node(&node.common, kind, bounds, has_explicit_box, imposed, path)?;
        scene_node.sizing = authored_sizing(&node.kind);
        Some(scene_node)
    }

    fn finish_node(
        &mut self,
        common: &CommonProperties,
        kind: SceneKind,
        bounds: Bounds,
        has_explicit_box: bool,
        imposed: Imposed,
        path: &str,
    ) -> Option<SceneNode> {
        let layer = common.id.as_str();
        let (x, y) = match imposed.position {
            Some(position) => position,
            None => {
                let x = match &common.x {
                    Some(value) => self.number(value, &format!("{path}.x"), layer)?,
                    None => 0.0,
                };
                let y = match &common.y {
                    Some(value) => self.number(value, &format!("{path}.y"), layer)?,
                    None => 0.0,
                };
                (x, y)
            }
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
            None if imposed.position.is_some() => (bounds.x, bounds.y),
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

/// `None` where the property is left out, which means hug.
fn authored_sizes(kind: &NodeKind) -> (Option<&SizeValue>, Option<&SizeValue>) {
    match kind {
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
    }
}

fn authored_sizing(kind: &NodeKind) -> Sizing {
    let (width, height) = authored_sizes(kind);
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

/// What a parent's layout decides for a child, overriding the child's own properties.
#[derive(Debug, Clone, Copy, Default)]
struct Imposed<'t, 'd> {
    width: Option<f64>,
    height: Option<f64>,
    /// Where the box's top-left goes in the parent.
    position: Option<(f64, f64)>,
    /// A layout group's node in the tree its outermost layout group computed.
    layout: Option<(&'t TaffyTree<Leaf<'d>>, NodeId)>,
    /// Asked for its content size: an axis without a size hugs, whatever is authored.
    measuring: bool,
}

impl Imposed<'_, '_> {
    fn extent(&self, axis: Axis) -> Option<f64> {
        match axis {
            Axis::Width => self.width,
            Axis::Height => self.height,
        }
    }
}

/// Content inside a layout that taffy asks the scene builder to measure.
#[derive(Debug)]
struct Leaf<'d> {
    node: &'d Node,
    path: String,
}

fn is_in_flow(node: &Node) -> bool {
    node.common.visible != Some(false) && node.common.position.is_none()
}

fn layout_of(node: &Node) -> Option<&document::Layout> {
    match &node.kind {
        NodeKind::Group(group) => group.properties.layout.as_ref(),
        _ => None,
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
    const NONE: Self = Self {
        width: (0.0, f64::INFINITY),
        height: (0.0, f64::INFINITY),
    };

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

    /// The top-left of an unrotated node's box in its parent.
    fn origin(node: &SceneNode) -> (f64, f64) {
        node.transform.apply(node.bounds.x, node.bounds.y)
    }

    fn rect(id: &str, width: &str, height: &str) -> String {
        format!(r#"{{ "id": "{id}", "type": "rect", "width": {width}, "height": {height} }}"#)
    }

    #[test]
    fn a_layout_group_hugs_its_children_plus_padding_and_gaps() {
        let scene = build_frame(&format!(
            r#"{{ "id": "card", "type": "group", "x": 5, "y": 5,
                  "layout": {{ "direction": "column", "gap": 8, "padding": 16 }},
                  "children": [{}, {}] }}"#,
            rect("small", "50", "10"),
            rect("large", "80", "20")
        ))
        .unwrap();
        let card = &scene.nodes[0];

        assert_eq!(card.bounds, Bounds::sized(112.0, 70.0));
        assert_eq!(origin(card), (5.0, 5.0));
        assert_eq!(origin(&children(card)[0]), (16.0, 16.0));
        assert_eq!(origin(&children(card)[1]), (16.0, 34.0));
    }

    #[test]
    fn fill_children_share_the_space_left_along_the_row() {
        let scene = build_frame(&format!(
            r#"{{ "id": "row", "type": "group", "width": "fill",
                  "layout": {{ "gap": 10 }}, "children": [{}, {}, {}] }}"#,
            rect("fixed", "50", "10"),
            rect("first", r#""fill""#, "20"),
            rect("second", r#""fill""#, "10")
        ))
        .unwrap();
        let row = children(&scene.nodes[0]);

        assert_eq!(scene.nodes[0].bounds, Bounds::sized(200.0, 20.0));
        assert_eq!((origin(&row[1]).0, row[1].bounds.width), (60.0, 65.0));
        assert_eq!((origin(&row[2]).0, row[2].bounds.width), (135.0, 65.0));
    }

    #[test]
    fn alignment_places_children_along_and_across_the_direction() {
        let scene = build_frame(&format!(
            r#"{{ "id": "centered", "type": "group", "width": 100, "height": 50,
                  "layout": {{ "align": "center", "crossAlign": "center" }}, "children": [{}] }},
               {{ "id": "spread", "type": "group", "width": 100,
                  "layout": {{ "align": "space-between", "crossAlign": "end" }},
                  "children": [{}, {}, {}] }}"#,
            rect("dot", "20", "10"),
            rect("a", "10", "10"),
            rect("b", "10", "30"),
            rect("c", "10", "10")
        ))
        .unwrap();

        assert_eq!(origin(&children(&scene.nodes[0])[0]), (40.0, 20.0));
        let spread: Vec<_> = children(&scene.nodes[1]).iter().map(origin).collect();
        assert_eq!(spread, [(0.0, 20.0), (45.0, 0.0), (90.0, 20.0)]);
    }

    #[test]
    fn filling_across_a_hugging_layout_stretches_to_the_largest_sibling() {
        let scene = build_frame(&format!(
            r#"{{ "id": "stack", "type": "group", "layout": {{ "direction": "column" }},
                  "children": [{}, {}] }}"#,
            rect("wide", "100", "10"),
            rect("rule", r#""fill""#, "2")
        ))
        .unwrap();

        assert_eq!(children(&scene.nodes[0])[1].bounds.width, 100.0);
    }

    #[test]
    fn text_filling_a_column_wraps_and_the_column_grows_to_fit() {
        let scene = build_frame(
            r##"{ "id": "card", "type": "group", "width": 120,
                  "layout": { "direction": "column", "padding": 10 }, "children": [
                { "id": "copy", "type": "text", "text": "Design as code, not coordinates",
                  "width": "fill", "font": { "file": "Tuffy.ttf", "size": 20 },
                  "fill": { "color": "#000" } } ] }"##,
        )
        .unwrap();
        let card = &scene.nodes[0];
        let copy = &children(card)[0];
        let SceneKind::Text { line_count, .. } = copy.kind else {
            panic!("copy is text");
        };

        assert!(line_count > 1);
        assert_eq!(copy.bounds.width, 100.0);
        assert_eq!(card.bounds.height, copy.bounds.height + 20.0);
    }

    #[test]
    fn hugging_text_in_a_row_keeps_its_line() {
        let scene = build_frame(
            r##"{ "id": "button", "type": "group", "layout": { "padding": [8, 16] }, "children": [
                { "id": "label", "type": "text", "text": "Get started",
                  "font": { "file": "Tuffy.ttf", "size": 17.3 }, "fill": { "color": "#000" } } ] }"##,
        )
        .unwrap();
        let label = &children(&scene.nodes[0])[0];
        let SceneKind::Text { line_count, .. } = label.kind else {
            panic!("label is text");
        };

        assert_eq!(line_count, 1);
        assert_eq!(origin(label), (16.0, 8.0));
    }

    #[test]
    fn absolute_and_hidden_children_leave_the_flow() {
        let scene = build_frame(&format!(
            r##"{{ "id": "card", "type": "group", "layout": {{ "gap": 10, "padding": 10 }},
                   "children": [
                {{ "id": "backdrop", "type": "rect", "position": "absolute",
                   "width": "fill", "height": "fill", "fill": {{ "color": "#FFF" }} }},
                {}, {{ "id": "hidden", "type": "rect", "width": 10, "height": 10,
                       "visible": false }}, {},
                {{ "id": "badge", "type": "rect", "position": "absolute", "x": 25, "y": -5,
                   "width": 10, "height": 10 }} ] }}"##,
            rect("first", "10", "20"),
            rect("second", "10", "20")
        ))
        .unwrap();
        let card = &scene.nodes[0];
        let layers = children(card);

        assert_eq!(card.bounds, Bounds::sized(50.0, 40.0));
        assert_eq!(layers[0].bounds, Bounds::sized(50.0, 40.0));
        assert_eq!(origin(&layers[3]), (30.0, 10.0));
        assert_eq!(origin(&layers[4]), (25.0, -5.0));
    }

    #[test]
    fn nested_layouts_and_rotation_leave_siblings_in_place() {
        let scene = build_frame(&format!(
            r#"{{ "id": "row", "type": "group", "width": 200, "layout": {{ "gap": 10 }},
                  "children": [
                {{ "id": "turned", "type": "rect", "width": 20, "height": 20, "rotation": 45 }},
                {{ "id": "column", "type": "group", "width": "fill",
                   "layout": {{ "direction": "column", "crossAlign": "end" }},
                   "children": [{}, {}] }} ] }}"#,
            rect("top", "10", "10"),
            rect("bottom", "30", "10")
        ))
        .unwrap();
        let row = children(&scene.nodes[0]);
        let column = children(&row[1]);

        assert_eq!((origin(&row[1]).0, row[1].bounds.width), (30.0, 170.0));
        assert_eq!(origin(&column[0]), (160.0, 0.0));
        assert_eq!(origin(&column[1]), (140.0, 10.0));
    }

    #[test]
    fn overflowing_a_fixed_layout_warns_with_the_amount() {
        let scene = build_frame(&format!(
            r#"{{ "id": "row", "type": "group", "width": 50, "layout": {{}},
                  "children": [{}, {}] }}"#,
            rect("a", "40", "10"),
            rect("b", "40", "10")
        ))
        .unwrap();

        let warning = scene
            .warnings
            .iter()
            .find(|warning| warning.code == "layout_overflow")
            .expect("an overflow warning");
        assert_eq!(warning.layer.as_deref(), Some("row"));
        assert!(
            warning.message.contains("30px horizontally"),
            "{}",
            warning.message
        );
    }

    #[test]
    fn a_group_without_layout_is_measured_by_its_children_inside_a_row() {
        let scene = build_frame(&format!(
            r#"{{ "id": "row", "type": "group", "layout": {{ "gap": 10 }}, "children": [
                {{ "id": "loose", "type": "group", "children": [
                    {{ "id": "dot", "type": "rect", "x": 5, "y": 5, "width": 10, "height": 10 }} ] }},
                {} ] }}"#,
            rect("next", "10", "10")
        ))
        .unwrap();
        let row = children(&scene.nodes[0]);

        assert_eq!(origin(&row[0]), (0.0, 0.0));
        assert_eq!(origin(&row[1]), (20.0, 0.0));
    }

    #[test]
    fn problems_inside_a_layout_are_reported_once() {
        let error = only_error(
            r##"{ "id": "row", "type": "group", "layout": {}, "children": [
                { "id": "copy", "type": "text", "text": "Hi",
                  "font": { "file": "Missing.ttf", "size": 20 }, "fill": { "color": "#000" } } ] }"##,
        );

        assert_eq!(error.layer.as_deref(), Some("copy"));
    }

    #[test]
    fn layout_mistakes_are_errors() {
        let placed = only_error(
            r#"{ "id": "row", "type": "group", "layout": {}, "children": [
                 { "id": "a", "type": "rect", "x": 4, "width": 10, "height": 10 } ] }"#,
        );
        assert_eq!(placed.path.as_deref(), Some("children[0].children[0].x"));
        assert!(placed.message.contains("absolute"), "{}", placed.message);

        let circular = only_error(&format!(
            r#"{{ "id": "row", "type": "group", "layout": {{}}, "children": [{}] }}"#,
            rect("a", r#""fill""#, "10")
        ));
        assert!(circular.message.contains("\"row\""), "{}", circular.message);

        let stray = only_error(
            r#"{ "id": "a", "type": "rect", "position": "absolute", "width": 1, "height": 1 }"#,
        );
        assert_eq!(stray.path.as_deref(), Some("children[0].position"));
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
