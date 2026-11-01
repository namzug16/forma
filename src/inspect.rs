//! Structural report of a resolved scene. Reads only; never touches the source.

use serde_json::{Map, Value, json};

use crate::diagnostics::{Diagnostic, Failure};
use crate::scene::{Affine, Bounds, NodeLayout, Scene, SceneKind, SceneNode};

/// The report for a whole frame, or for one layer when `layer_id` is given.
pub fn report(scene: &Scene, layer_id: Option<&str>) -> Result<Value, Failure> {
    let mut draw_order = 0;
    let mut layers: Vec<Value> = describe_nodes(
        &scene.nodes,
        Affine::IDENTITY,
        scene.frame_area(),
        Inherited::ROOT,
        &mut draw_order,
    );
    if let Some(layer_id) = layer_id {
        let layer = find_layer(&layers, layer_id).cloned().ok_or_else(|| {
            Diagnostic::new("layer_not_found", format!("no layer has id \"{layer_id}\""))
        })?;
        layers = vec![layer];
    }
    Ok(json!({
        "frame": scene.id,
        "width": scene.width,
        "height": scene.height,
        "background": scene.background,
        "warnings": scene.warnings,
        "layers": layers,
    }))
}

fn find_layer<'a>(layers: &'a [Value], layer_id: &str) -> Option<&'a Value> {
    layers.iter().find_map(|layer| {
        if layer["id"] == layer_id {
            return Some(layer);
        }
        find_layer(layer["children"].as_array()?, layer_id)
    })
}

/// What a node takes from its ancestors.
#[derive(Clone, Copy)]
struct Inherited {
    opacity: f64,
    visible: bool,
}

impl Inherited {
    const ROOT: Self = Self {
        opacity: 1.0,
        visible: true,
    };
}

fn describe_nodes(
    nodes: &[SceneNode],
    parent: Affine,
    visible_area: Bounds,
    inherited: Inherited,
    draw_order: &mut usize,
) -> Vec<Value> {
    nodes
        .iter()
        .map(|node| describe_node(node, parent, visible_area, inherited, draw_order))
        .collect()
}

fn describe_node(
    node: &SceneNode,
    parent: Affine,
    visible_area: Bounds,
    inherited: Inherited,
    draw_order: &mut usize,
) -> Value {
    let layout = NodeLayout::of(node, parent, visible_area);
    let effective = Inherited {
        opacity: inherited.opacity * node.opacity,
        visible: inherited.visible && node.visible,
    };
    let mut layer = json!({
        "id": node.id,
        "drawOrder": *draw_order,
        "visible": node.visible,
        "effectiveVisible": effective.visible,
        "opacity": node.opacity,
        "effectiveOpacity": effective.opacity,
        "localTransform": node.transform,
        "worldTransform": layout.world,
        "localBounds": node.bounds,
        "worldBounds": layout.world_bounds,
        "clippedBounds": layout.clipped_bounds,
        "overflowsClip": layout.overflows_clip(),
    });
    *draw_order += 1;
    let details = layer.as_object_mut().expect("layer is a JSON object");
    match &node.kind {
        SceneKind::Rect {
            corner_radius,
            fill,
            stroke,
        } => {
            insert(details, "type", json!("rect"));
            insert(details, "cornerRadius", json!(corner_radius));
            insert(details, "fill", json!(fill));
            insert(details, "stroke", json!(stroke));
        }
        SceneKind::Ellipse { fill, stroke } => {
            insert(details, "type", json!("ellipse"));
            insert(details, "fill", json!(fill));
            insert(details, "stroke", json!(stroke));
        }
        SceneKind::Line { stroke } => {
            insert(details, "type", json!("line"));
            insert(details, "stroke", json!(stroke));
        }
        SceneKind::Text {
            outlines,
            fill,
            font,
            font_source,
            line_count,
        } => {
            insert(details, "type", json!("text"));
            insert(details, "fill", json!(fill));
            insert(details, "font", json!(font));
            insert(details, "fontSource", json!(font_source));
            insert(details, "lineCount", json!(line_count));
            let ink_bounds = outlines.as_ref().map(|outlines| {
                let bounds = outlines.bounds();
                json!({
                    "x": bounds.x(),
                    "y": bounds.y(),
                    "width": bounds.width(),
                    "height": bounds.height(),
                })
            });
            insert(details, "inkBounds", json!(ink_bounds));
        }
        SceneKind::Image { source, .. } => {
            insert(details, "type", json!("image"));
            insert(details, "source", json!(source));
        }
        SceneKind::Svg { source, color, .. } => {
            insert(details, "type", json!("svg"));
            insert(details, "source", json!(source));
            insert(details, "color", json!(color));
        }
        SceneKind::Group {
            children,
            clip,
            mask,
        } => {
            insert(details, "type", json!("group"));
            insert(details, "clip", json!(clip));
            insert(details, "mask", json!(mask.as_ref().map(|mask| &mask.id)));
            let children = describe_nodes(
                children,
                layout.world,
                layout.children_visible_area(*clip, visible_area),
                effective,
                draw_order,
            );
            insert(details, "children", json!(children));
        }
    }
    layer
}

fn insert(details: &mut Map<String, Value>, key: &str, value: Value) {
    details.insert(key.to_owned(), value);
}

/// Indented outline for people; `--json` is the full report.
pub fn print_outline(report: &Value) {
    println!(
        "{}  {}×{}",
        report["frame"].as_str().unwrap_or_default(),
        report["width"].as_f64().unwrap_or_default(),
        report["height"].as_f64().unwrap_or_default(),
    );
    print_layers(items_of(&report["layers"]), 1);
    for warning in items_of(&report["warnings"]) {
        println!(
            "warning: {}",
            warning["message"].as_str().unwrap_or_default()
        );
    }
}

fn items_of(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

fn print_layers(layers: &[Value], depth: usize) {
    for layer in layers {
        let bounds = &layer["clippedBounds"];
        let placement = if bounds.is_null() {
            "entirely clipped".to_owned()
        } else {
            format!(
                "x={:.1} y={:.1} {:.1}×{:.1}",
                bounds["x"].as_f64().unwrap_or_default(),
                bounds["y"].as_f64().unwrap_or_default(),
                bounds["width"].as_f64().unwrap_or_default(),
                bounds["height"].as_f64().unwrap_or_default(),
            )
        };
        println!(
            "{}#{} {} \"{}\"  {placement}",
            "  ".repeat(depth),
            layer["drawOrder"],
            layer["type"].as_str().unwrap_or_default(),
            layer["id"].as_str().unwrap_or_default(),
        );
        print_layers(items_of(&layer["children"]), depth + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::Assets;
    use crate::document::parse_frame;
    use crate::scene::{Resources, build};
    use crate::text::FontLibrary;
    use crate::tokens::Tokens;

    fn scene_with_hidden_group() -> Scene {
        let frame = r##"{ "version": 1, "id": "f", "width": 10, "height": 10, "children": [
            { "id": "hidden", "type": "group", "visible": false, "children": [
                { "id": "offscreen", "type": "rect", "x": 50, "width": 5, "height": 5,
                  "fill": { "color": "#FF0000" } } ] } ] }"##;
        let resources = Resources {
            tokens: Tokens::parse("{}").unwrap(),
            assets: Assets::new("/no/assets".into()),
            fonts: FontLibrary::default(),
        };
        build(&parse_frame(frame).unwrap(), &resources).unwrap()
    }

    #[test]
    fn a_child_reports_being_hidden_by_its_group() {
        let report = report(&scene_with_hidden_group(), Some("offscreen")).unwrap();

        assert_eq!(report["layers"][0]["visible"], true);
        assert_eq!(report["layers"][0]["effectiveVisible"], false);
    }

    #[test]
    fn hidden_groups_do_not_warn_about_clipped_descendants() {
        assert!(scene_with_hidden_group().warnings.is_empty());
    }
}
