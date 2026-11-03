//! The details of one layer: what forma resolved it to, and what the frame file says.

use std::collections::HashMap;

use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::viewer::{describe_paint, detail_section, value_chip};

/// One leaf of a layer as written in its frame file, such as `fill.color`.
#[derive(Clone, Debug, PartialEq)]
pub struct AuthoredProperty {
    pub name: String,
    pub value: String,
    /// What a `$token` reference resolves to; `None` for literal values.
    pub resolved: Option<String>,
}

/// Every layer's authored properties by id; ids are unique within a frame.
pub fn authored_properties(
    frame: &Value,
    tokens: &Value,
) -> HashMap<String, Vec<AuthoredProperty>> {
    let mut layers = HashMap::new();
    collect_layers(&frame["children"], tokens, &mut layers);
    layers
}

fn collect_layers(
    nodes: &Value,
    tokens: &Value,
    layers: &mut HashMap<String, Vec<AuthoredProperty>>,
) {
    let nodes = match nodes {
        Value::Array(nodes) => nodes.as_slice(),
        // A group's mask is a single node.
        node @ Value::Object(_) => std::slice::from_ref(node),
        _ => return,
    };
    for node in nodes {
        let Some(properties) = node.as_object() else {
            continue;
        };
        let mut leaves = Vec::new();
        for (name, value) in properties {
            if !matches!(name.as_str(), "id" | "type" | "children" | "mask") {
                push_leaves(name.clone(), value, tokens, &mut leaves);
            }
        }
        if let Some(id) = node["id"].as_str() {
            layers.insert(id.to_owned(), leaves);
        }
        collect_layers(&node["children"], tokens, layers);
        collect_layers(&node["mask"], tokens, layers);
    }
}

fn push_leaves(name: String, value: &Value, tokens: &Value, leaves: &mut Vec<AuthoredProperty>) {
    let is_branch = |value: &Value| value.is_object() || value.is_array();
    match value {
        Value::Object(fields) => {
            for (key, field) in fields {
                push_leaves(format!("{name}.{key}"), field, tokens, leaves);
            }
        }
        Value::Array(items) if items.iter().any(is_branch) => {
            for (index, item) in items.iter().enumerate() {
                push_leaves(format!("{name}.{index}"), item, tokens, leaves);
            }
        }
        _ => leaves.push(AuthoredProperty {
            name,
            value: display_value(value),
            resolved: value
                .as_str()
                .and_then(|text| resolve_token(text, tokens))
                .map(display_value),
        }),
    }
}

/// `$colors.primary` looked up in the resolved tokens.
fn resolve_token<'a>(text: &str, tokens: &'a Value) -> Option<&'a Value> {
    let path = text.strip_prefix('$')?;
    path.split('.')
        .try_fold(tokens, |value, key| value.get(key))
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => format_number(number.as_f64().unwrap_or_default()),
        Value::Null => "none".to_owned(),
        Value::Array(items) => {
            let items: Vec<_> = items.iter().map(display_value).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(fields) => fields
            .iter()
            .map(|(key, field)| format!("{key} {}", display_value(field)))
            .collect::<Vec<_>>()
            .join(" · "),
        Value::Bool(flag) => flag.to_string(),
    }
}

/// At most two decimals, without trailing zeros.
pub fn format_number(number: f64) -> String {
    let rounded = (number * 100.).round() / 100.;
    // Avoids "-0" for tiny negative values.
    format!("{}", rounded + 0.)
}

/// How a transform reads to a person, or `None` when it only moves the layer. A skewed transform
/// can't be split into these, so it is shown as its matrix.
fn describe_transform(transform: &Value) -> Option<Vec<(&'static str, String)>> {
    let entry = |key: &str| transform[key].as_f64();
    let (a, b, c, d) = (entry("a")?, entry("b")?, entry("c")?, entry("d")?);
    let scale_x = a.hypot(b);
    let skew = (a * c + b * d) / scale_x;
    let scale_y = (a * d - b * c) / scale_x;
    let rotation = b.atan2(a).to_degrees();
    if skew.abs() > 1e-6 {
        let matrix = [a, b, c, d, entry("e")?, entry("f")?].map(format_number);
        return Some(vec![("Matrix", matrix.join(" "))]);
    }
    let mut parts = Vec::new();
    if rotation.abs() > 1e-6 {
        parts.push(("Rotate", format!("{}°", format_number(rotation))));
    }
    if (scale_x - 1.).abs() > 1e-6 || (scale_y - 1.).abs() > 1e-6 {
        parts.push((
            "Scale",
            format!("{} × {}", format_number(scale_x), format_number(scale_y)),
        ));
    }
    (!parts.is_empty()).then_some(parts)
}

fn chip_row(chips: impl IntoIterator<Item = AnyElement>) -> Div {
    h_flex().gap_2().children(chips)
}

fn chip(label: &str, value: String, cx: &App) -> AnyElement {
    value_chip(label, None, value, cx).into_any_element()
}

fn subheading(title: &str, cx: &App) -> Div {
    div()
        .pt_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(title.to_owned())
}

pub fn render(layer: &Value, authored: &[AuthoredProperty], cx: &App) -> Vec<AnyElement> {
    let number = |value: &Value| format_number(value.as_f64().unwrap_or_default());
    let bounds = &layer["worldBounds"];
    let mut resolved = detail_section(layer["id"].as_str().unwrap_or_default(), cx)
        .child(subheading("Position & Size", cx))
        .child(chip_row([
            chip("X", number(&bounds["x"]), cx),
            chip("Y", number(&bounds["y"]), cx),
        ]))
        .child(chip_row([
            chip("W", number(&bounds["width"]), cx),
            chip("H", number(&bounds["height"]), cx),
        ]));
    let local_transform = describe_transform(&layer["localTransform"]);
    let world_transform = describe_transform(&layer["worldTransform"]);
    for (title, parts) in [
        ("Transform", &local_transform),
        ("World transform", &world_transform),
    ] {
        let is_inherited_only = title == "World transform" && world_transform == local_transform;
        if let Some(parts) = parts.as_ref().filter(|_| !is_inherited_only) {
            resolved = resolved.child(subheading(title, cx)).child(chip_row(
                parts
                    .iter()
                    .map(|(label, value)| chip(label, value.clone(), cx)),
            ));
        }
    }

    let percent =
        |value: &Value| format!("{}%", format_number(value.as_f64().unwrap_or(1.) * 100.));
    let mut appearance = vec![chip("Opacity", percent(&layer["opacity"]), cx)];
    if layer["effectiveOpacity"] != layer["opacity"] {
        appearance.push(chip("Effective", percent(&layer["effectiveOpacity"]), cx));
    }
    resolved = resolved
        .child(subheading("Appearance", cx))
        .child(chip_row(appearance));
    if layer["effectiveVisible"] == false {
        let reason = if layer["visible"] == false {
            "Hidden"
        } else {
            "Hidden by a parent"
        };
        resolved = resolved.child(chip_row([chip("", reason.to_owned(), cx)]));
    }
    if !layer["fill"].is_null() {
        let (swatch, description) = describe_paint(&layer["fill"]);
        resolved = resolved.child(chip_row([
            value_chip("Fill", swatch, description, cx).into_any_element()
        ]));
    }
    let stroke = &layer["stroke"];
    if !stroke.is_null() {
        let (swatch, description) =
            describe_paint(&serde_json::json!({ "type": "solid", "color": stroke["color"] }));
        resolved = resolved.child(chip_row([
            value_chip("Stroke", swatch, description, cx).into_any_element(),
            chip("W", number(&stroke["width"]), cx),
        ]));
    }
    if let Some(radius) = layer.get("cornerRadius") {
        resolved = resolved.child(chip_row([chip("Radius", number(radius), cx)]));
    }

    let mut specifics = Vec::new();
    match layer["type"].as_str().unwrap_or_default() {
        "text" => {
            let font = &layer["font"];
            let family = font["family"]
                .as_str()
                .or(font["file"].as_str())
                .unwrap_or_default();
            specifics.push(chip_row([chip("Font", family.to_owned(), cx)]));
            specifics.push(chip_row([
                chip("Weight", number(&font["weight"]), cx),
                chip("Size", number(&font["size"]), cx),
            ]));
            specifics.push(chip_row([
                chip("Line height", number(&font["lineHeight"]), cx),
                chip("Lines", number(&layer["lineCount"]), cx),
            ]));
            if let Some(source) = layer["fontSource"].as_str() {
                specifics.push(chip_row([chip("Source", source.to_owned(), cx)]));
            }
            let ink = &layer["inkBounds"];
            if !ink.is_null() {
                specifics.push(chip_row([
                    chip("Ink W", number(&ink["width"]), cx),
                    chip("Ink H", number(&ink["height"]), cx),
                ]));
            }
        }
        "image" | "svg" => {
            specifics.push(chip_row([chip(
                "Source",
                display_value(&layer["source"]),
                cx,
            )]));
            if let Some(color) = layer["color"].as_str() {
                let (swatch, description) =
                    describe_paint(&serde_json::json!({ "type": "solid", "color": color }));
                specifics.push(chip_row([
                    value_chip("Color", swatch, description, cx).into_any_element()
                ]));
            }
        }
        "group" => {
            specifics.push(chip_row([
                chip("Clip", display_value(&layer["clip"]), cx),
                chip("Mask", display_value(&layer["mask"]), cx),
            ]));
        }
        _ => {}
    }
    if !specifics.is_empty() {
        let kind = layer["type"].as_str().unwrap_or_default();
        let mut title = kind.to_owned();
        title[..1.min(kind.len())].make_ascii_uppercase();
        resolved = resolved.child(subheading(&title, cx)).children(specifics);
    }

    let mut sections = vec![resolved.into_any_element()];
    if !authored.is_empty() {
        sections.push(
            detail_section("In the frame file", cx)
                .gap_1()
                .children(
                    authored
                        .iter()
                        .map(|property| render_authored(property, cx)),
                )
                .into_any_element(),
        );
    }
    sections
}

fn render_authored(property: &AuthoredProperty, cx: &App) -> impl IntoElement {
    h_flex()
        .items_start()
        .gap_2()
        .text_xs()
        .child(
            div()
                .w(relative(0.4))
                .flex_none()
                .truncate()
                .text_color(cx.theme().muted_foreground)
                .child(property.name.clone()),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .child(property.value.clone())
                .when_some(property.resolved.clone(), |value, resolved| {
                    value.child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("→ {resolved}")),
                    )
                }),
        )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{AuthoredProperty, authored_properties, describe_transform, format_number};

    #[test]
    fn nested_properties_flatten_and_token_references_resolve() {
        let frame = json!({ "children": [{
            "id": "card", "type": "rect", "x": "$spacing.lg", "anchor": [0.5, 0.5],
            "fill": { "color": "$colors.primary" },
            "children": [{ "id": "inner", "width": 10 }]
        }] });
        let tokens = json!({ "spacing": { "lg": 64 }, "colors": { "primary": "#E8FF57" } });

        let layers = authored_properties(&frame, &tokens);

        let property = |name: &str, value: &str, resolved: Option<&str>| AuthoredProperty {
            name: name.to_owned(),
            value: value.to_owned(),
            resolved: resolved.map(str::to_owned),
        };
        assert_eq!(
            layers["card"],
            [
                property("x", "$spacing.lg", Some("64")),
                property("anchor", "[0.5, 0.5]", None),
                property("fill.color", "$colors.primary", Some("#E8FF57")),
            ]
        );
        assert_eq!(layers["inner"], [property("width", "10", None)]);
    }

    #[test]
    fn a_translation_alone_is_not_worth_describing() {
        let moved = json!({ "a": 1, "b": 0, "c": 0, "d": 1, "e": 64, "f": 100 });

        assert_eq!(describe_transform(&moved), None);
    }

    #[test]
    fn rotation_and_scale_are_read_back_out() {
        let (sin, cos) = 30f64.to_radians().sin_cos();
        let rotated = json!({ "a": 2. * cos, "b": 2. * sin, "c": -sin, "d": cos, "e": 0, "f": 0 });

        assert_eq!(
            describe_transform(&rotated),
            Some(vec![
                ("Rotate", "30°".to_owned()),
                ("Scale", "2 × 1".to_owned())
            ])
        );
    }

    #[test]
    fn skew_falls_back_to_the_matrix() {
        let skewed = json!({ "a": 1, "b": 0, "c": 0.5, "d": 1, "e": 0, "f": 0 });

        assert_eq!(
            describe_transform(&skewed),
            Some(vec![("Matrix", "1 0 0.5 1 0 0".to_owned())])
        );
    }

    #[test]
    fn numbers_keep_two_decimals_at_most() {
        assert_eq!(format_number(1.0), "1");
        assert_eq!(format_number(464.0160440463835), "464.02");
        assert_eq!(format_number(-0.0001), "0");
    }
}
