//! The JSON source format. Nothing here knows how a frame is rendered.

use std::collections::HashSet;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::diagnostics::{Diagnostic, Failure};

pub const SUPPORTED_DOCUMENT_VERSION: u32 = 1;
const MAX_NESTING_DEPTH: usize = 64;

const COMMON_PROPERTIES: [&str; 15] = [
    "id",
    "x",
    "y",
    "anchor",
    "pivot",
    "rotation",
    "scaleX",
    "scaleY",
    "opacity",
    "visible",
    "minWidth",
    "maxWidth",
    "minHeight",
    "maxHeight",
    "position",
];
const ROOT_TRANSFORM_PROPERTIES: [&str; 7] =
    ["x", "y", "anchor", "pivot", "rotation", "scaleX", "scaleY"];
const FUTURE_PROPERTIES: [&str; 3] = ["mask", "effects", "blendMode"];

/// A literal number or a `$token` reference.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged, expecting = "a number or a $token reference")]
pub enum NumberValue {
    Literal(f64),
    Token(String),
}

/// A box size: a number, a `$token`, `"hug"`, `"fill"` or a percentage such as `"50%"`.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "NumberValue")]
pub enum SizeValue {
    Number(NumberValue),
    /// Fit the content.
    Hug,
    /// Take the parent's whole size on this axis.
    Fill,
    /// A share of the parent's size, `50.0` for `"50%"`.
    Percent(f64),
}

impl TryFrom<NumberValue> for SizeValue {
    type Error = String;

    fn try_from(value: NumberValue) -> Result<Self, Self::Error> {
        let NumberValue::Token(text) = &value else {
            return Ok(Self::Number(value));
        };
        let percent = text
            .strip_suffix('%')
            .and_then(|number| number.parse::<f64>().ok())
            .filter(|number| number.is_finite() && *number >= 0.0);
        match (text.as_str(), percent) {
            ("hug", _) => Ok(Self::Hug),
            ("fill", _) => Ok(Self::Fill),
            (_, Some(percent)) => Ok(Self::Percent(percent)),
            (token, _) if token.starts_with('$') => Ok(Self::Number(value)),
            (other, _) => Err(format!(
                "expected a number, a $token, \"hug\", \"fill\" or a percentage, found \"{other}\""
            )),
        }
    }
}

impl JsonSchema for SizeValue {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SizeValue".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "A number, a $token reference, \"hug\", \"fill\" or a percentage of the parent such as \"50%\".",
            "anyOf": [
                { "type": "number" },
                { "type": "string", "pattern": "^(\\$.+|hug|fill|[0-9]+(\\.[0-9]+)?%)$" }
            ]
        })
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SolidFill {
    pub color: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GradientStop {
    pub offset: f64,
    pub color: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinearGradient {
    /// Degrees; 0 points up, 90 points right.
    pub angle: f64,
    pub stops: Vec<GradientStop>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RadialGradient {
    /// Normalized to the node's width and height.
    pub center: [f64; 2],
    /// Normalized per axis: the ellipse radii are radius×width and radius×height.
    pub radius: f64,
    pub stops: Vec<GradientStop>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Gradient {
    LinearGradient(LinearGradient),
    RadialGradient(RadialGradient),
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(
    untagged,
    expecting = "a fill: {\"color\": ...}, a gradient object, or a $token reference"
)]
pub enum Fill {
    Token(String),
    Solid(SolidFill),
    Gradient(Gradient),
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Stroke {
    pub color: String,
    pub width: NumberValue,
    pub opacity: Option<f64>,
    pub cap: Option<LineCap>,
    pub join: Option<LineJoin>,
    /// Alternating dash and gap lengths.
    pub dash: Option<Vec<f64>>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FontStyle {
    #[default]
    Normal,
    Italic,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Font {
    /// System family name; ignored for shaping when `file` is set.
    pub family: Option<String>,
    /// Font file relative to the assets directory, e.g. `fonts/Inter.ttf`.
    pub file: Option<String>,
    pub size: NumberValue,
    pub weight: Option<u16>,
    pub style: Option<FontStyle>,
    /// Multiple of `size`.
    pub line_height: Option<f64>,
    /// Logical pixels added after each glyph.
    pub letter_spacing: Option<f64>,
    /// Families explicitly allowed when earlier fonts lack the text's glyphs.
    pub fallbacks: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged, expecting = "a font object or a $typography token reference")]
pub enum FontValue {
    Token(String),
    Inline(Font),
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ImageFit {
    #[default]
    Cover,
    Contain,
    Fill,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommonProperties {
    pub id: String,
    pub x: Option<NumberValue>,
    pub y: Option<NumberValue>,
    pub anchor: Option<[f64; 2]>,
    pub pivot: Option<[f64; 2]>,
    /// Degrees, clockwise.
    pub rotation: Option<f64>,
    pub scale_x: Option<f64>,
    pub scale_y: Option<f64>,
    pub opacity: Option<f64>,
    pub visible: Option<bool>,
    /// Limits applied after `width` and `height` are resolved, whether hug, fill or fixed.
    pub min_width: Option<NumberValue>,
    pub max_width: Option<NumberValue>,
    pub min_height: Option<NumberValue>,
    pub max_height: Option<NumberValue>,
    /// `absolute` takes a child of a layout group out of the flow, back to `x` and `y`.
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    Absolute,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[default]
    Row,
    Column,
}

/// Where children sit along the layout direction.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum MainAlign {
    #[default]
    Start,
    Center,
    End,
    SpaceBetween,
}

/// Where children sit across the layout direction.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CrossAlign {
    #[default]
    Start,
    Center,
    End,
    Stretch,
}

/// One number for every side, `[vertical, horizontal]`, or `[top, right, bottom, left]`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(
    untagged,
    expecting = "a number, [vertical, horizontal] or [top, right, bottom, left]"
)]
pub enum Padding {
    All(NumberValue),
    Axes([NumberValue; 2]),
    Sides([NumberValue; 4]),
}

/// Places the group's children in a row or column instead of at their `x` and `y`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Layout {
    pub direction: Option<Direction>,
    /// Space between neighboring children.
    pub gap: Option<NumberValue>,
    pub padding: Option<Padding>,
    pub align: Option<MainAlign>,
    pub cross_align: Option<CrossAlign>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RectProperties {
    pub width: SizeValue,
    pub height: SizeValue,
    pub corner_radius: Option<NumberValue>,
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EllipseProperties {
    pub width: SizeValue,
    pub height: SizeValue,
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
}

/// Runs from the box's top-left corner to its bottom-right corner.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LineProperties {
    pub width: SizeValue,
    pub height: SizeValue,
    pub stroke: Stroke,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextProperties {
    pub text: String,
    /// Wrapping width; omit or `"hug"` to wrap only at explicit newlines, or at `maxWidth`.
    pub width: Option<SizeValue>,
    pub font: FontValue,
    pub align: Option<TextAlign>,
    pub fill: Fill,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImageProperties {
    /// Path relative to the assets directory.
    pub src: String,
    pub width: SizeValue,
    pub height: SizeValue,
    pub fit: Option<ImageFit>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SvgProperties {
    /// Path relative to the assets directory.
    pub src: String,
    pub width: SizeValue,
    pub height: SizeValue,
    /// Replaces `currentColor`; colors hard-coded in the file stay as they are.
    pub color: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GroupProperties {
    /// Omitted or `"hug"` takes the box around the children.
    pub width: Option<SizeValue>,
    pub height: Option<SizeValue>,
    /// Clip children to the group's box, following `cornerRadius`.
    pub clip: Option<bool>,
    pub layout: Option<Layout>,
    /// The background box is painted behind the children.
    pub corner_radius: Option<NumberValue>,
    pub fill: Option<Fill>,
    pub stroke: Option<Stroke>,
}

#[derive(Debug, JsonSchema)]
pub struct GroupNode {
    #[schemars(flatten)]
    pub properties: GroupProperties,
    /// Drawn in the group's coordinates; its alpha is multiplied into the composited group.
    pub mask: Option<Box<Node>>,
    pub children: Vec<Node>,
}

#[derive(Debug, JsonSchema)]
#[schemars(tag = "type", rename_all = "lowercase")]
pub enum NodeKind {
    Rect(RectProperties),
    Ellipse(EllipseProperties),
    Line(LineProperties),
    Text(TextProperties),
    Image(ImageProperties),
    Svg(SvgProperties),
    Group(GroupNode),
}

#[derive(Debug, JsonSchema)]
pub struct Node {
    #[schemars(flatten)]
    pub common: CommonProperties,
    #[schemars(flatten)]
    pub kind: NodeKind,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrameHeader {
    pub version: u32,
    pub id: String,
    pub width: NumberValue,
    pub height: NumberValue,
    pub background: Option<Fill>,
}

/// The root of a document: one file is one frame.
#[derive(Debug, JsonSchema)]
pub struct Frame {
    #[schemars(flatten)]
    pub header: FrameHeader,
    pub children: Vec<Node>,
}

pub fn json_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(Frame)).expect("schemas serialize to JSON")
}

pub fn parse_frame(text: &str) -> Result<Frame, Failure> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Diagnostic::new("invalid_json", error.to_string()))?;
    let Value::Object(mut root) = value else {
        return Err(Diagnostic::new("invalid_document", "a frame must be a JSON object").into());
    };

    let mut parser = Parser::default();
    for property in ROOT_TRANSFORM_PROPERTIES {
        if root.contains_key(property) {
            parser.diagnostics.push(
                Diagnostic::new(
                    "unsupported_root_transform",
                    "the root frame cannot be positioned or transformed",
                )
                .at(property),
            );
        }
    }
    let children = root.remove("children");
    if children.is_none() {
        parser
            .diagnostics
            .push(Diagnostic::new("missing_property", "missing field `children`").at("children"));
    }
    let header = parser.deserialize::<FrameHeader>(root, "", None);
    if let Some(header) = &header
        && header.version != SUPPORTED_DOCUMENT_VERSION
    {
        parser.diagnostics.push(
            Diagnostic::new(
                "unsupported_version",
                format!("document version {} is not supported", header.version),
            )
            .at("version"),
        );
    }
    let children = parser.parse_children(children, "children", 1);

    match header {
        Some(header) if parser.diagnostics.is_empty() => Ok(Frame { header, children }),
        _ => Err(Failure(parser.diagnostics)),
    }
}

#[derive(Default)]
struct Parser {
    diagnostics: Vec<Diagnostic>,
    seen_ids: HashSet<String>,
}

impl Parser {
    fn deserialize<T: DeserializeOwned>(
        &mut self,
        properties: Map<String, Value>,
        base_path: &str,
        layer: Option<&str>,
    ) -> Option<T> {
        match serde_path_to_error::deserialize(Value::Object(properties)) {
            Ok(parsed) => Some(parsed),
            Err(error) => {
                let field_path = error.path().to_string();
                let message = error.into_inner().to_string();
                // shortcut: serde exposes error kinds only as text, replace if serde adds variants
                let code = if message.starts_with("unknown field") {
                    "unknown_property"
                } else if message.starts_with("missing field") {
                    "missing_property"
                } else {
                    "invalid_value"
                };
                let path = match (base_path, field_path.as_str()) {
                    (base, ".") => base.to_owned(),
                    ("", field) => field.to_owned(),
                    (base, field) => format!("{base}.{field}"),
                };
                let mut diagnostic = Diagnostic::new(code, message).at(path);
                diagnostic.layer = layer.map(str::to_owned);
                self.diagnostics.push(diagnostic);
                None
            }
        }
    }

    fn parse_children(&mut self, value: Option<Value>, path: &str, depth: usize) -> Vec<Node> {
        let items = match value {
            None => return Vec::new(),
            Some(Value::Array(items)) => items,
            Some(_) => {
                self.diagnostics
                    .push(Diagnostic::new("invalid_value", "`children` must be an array").at(path));
                return Vec::new();
            }
        };
        if depth > MAX_NESTING_DEPTH {
            self.diagnostics.push(
                Diagnostic::new(
                    "nesting_too_deep",
                    format!("groups may nest at most {MAX_NESTING_DEPTH} levels"),
                )
                .at(path),
            );
            return Vec::new();
        }
        items
            .into_iter()
            .enumerate()
            .filter_map(|(index, item)| self.parse_node(item, &format!("{path}[{index}]"), depth))
            .collect()
    }

    fn parse_node(&mut self, value: Value, path: &str, depth: usize) -> Option<Node> {
        let Value::Object(mut properties) = value else {
            self.diagnostics
                .push(Diagnostic::new("invalid_value", "a node must be an object").at(path));
            return None;
        };
        let layer = properties
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let layer = layer.as_deref();
        let node_type = properties.remove("type");

        let is_group = node_type.as_ref().and_then(Value::as_str) == Some("group");
        for feature in FUTURE_PROPERTIES {
            if is_group && feature == "mask" {
                continue;
            }
            if properties.remove(feature).is_some() {
                self.push(
                    Diagnostic::new(
                        "unsupported_feature",
                        format!("`{feature}` is planned for a later version"),
                    )
                    .at(format!("{path}.{feature}")),
                    layer,
                );
            }
        }

        let (common_properties, kind_properties): (Map<_, _>, Map<_, _>) = properties
            .into_iter()
            .partition(|(key, _)| COMMON_PROPERTIES.contains(&key.as_str()));
        let common = self.deserialize::<CommonProperties>(common_properties, path, layer);
        if let Some(common) = &common
            && !self.seen_ids.insert(common.id.clone())
        {
            self.push(
                Diagnostic::new(
                    "duplicate_node_id",
                    format!("node id \"{}\" is already used in this frame", common.id),
                )
                .at(format!("{path}.id")),
                layer,
            );
        }

        let kind = match node_type.as_ref().and_then(Value::as_str) {
            Some("rect") => self
                .deserialize(kind_properties, path, layer)
                .map(NodeKind::Rect),
            Some("ellipse") => self
                .deserialize(kind_properties, path, layer)
                .map(NodeKind::Ellipse),
            Some("line") => self
                .deserialize(kind_properties, path, layer)
                .map(NodeKind::Line),
            Some("text") => self
                .deserialize(kind_properties, path, layer)
                .map(NodeKind::Text),
            Some("image") => self
                .deserialize(kind_properties, path, layer)
                .map(NodeKind::Image),
            Some("svg") => self
                .deserialize(kind_properties, path, layer)
                .map(NodeKind::Svg),
            Some("group") => self.parse_group(kind_properties, path, depth, layer),
            Some("path") => {
                self.push(
                    Diagnostic::new("unsupported_feature", "`path` nodes are planned for V2")
                        .at(format!("{path}.type")),
                    layer,
                );
                None
            }
            Some(other) => {
                self.push(
                    Diagnostic::new(
                        "unknown_node_type",
                        format!("unknown node type \"{other}\""),
                    )
                    .at(format!("{path}.type")),
                    layer,
                );
                None
            }
            None => {
                self.push(
                    Diagnostic::new("missing_property", "a node needs a string `type`")
                        .at(format!("{path}.type")),
                    layer,
                );
                None
            }
        };
        Some(Node {
            common: common?,
            kind: kind?,
        })
    }

    fn parse_group(
        &mut self,
        mut properties: Map<String, Value>,
        path: &str,
        depth: usize,
        layer: Option<&str>,
    ) -> Option<NodeKind> {
        let children = properties.remove("children");
        let mask = properties.remove("mask");
        let group_properties = self.deserialize(properties, path, layer);
        let parsed_mask =
            mask.map(|mask| self.parse_node(mask, &format!("{path}.mask"), depth + 1));
        let children = self.parse_children(children, &format!("{path}.children"), depth + 1);
        let mask = match parsed_mask {
            Some(parsed) => Some(Box::new(parsed?)),
            None => None,
        };
        Some(NodeKind::Group(GroupNode {
            properties: group_properties?,
            mask,
            children,
        }))
    }

    fn push(&mut self, mut diagnostic: Diagnostic, layer: Option<&str>) {
        diagnostic.layer = layer.map(str::to_owned);
        self.diagnostics.push(diagnostic);
    }
}
