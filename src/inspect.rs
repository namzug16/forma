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
        1.0,
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

fn describe_nodes(
    nodes: &[SceneNode],
    parent: Affine,
    visible_area: Bounds,
    parent_opacity: f64,
    draw_order: &mut usize,
) -> Vec<Value> {
    nodes
        .iter()
        .map(|node| describe_node(node, parent, visible_area, parent_opacity, draw_order))
        .collect()
}

fn describe_node(
    node: &SceneNode,
    parent: Affine,
    visible_area: Bounds,
    parent_opacity: f64,
    draw_order: &mut usize,
) -> Value {
    let layout = NodeLayout::of(node, parent, visible_area);
    let effective_opacity = parent_opacity * node.opacity;
    let mut layer = json!({
        "id": node.id,
        "drawOrder": *draw_order,
        "visible": node.visible,
        "opacity": node.opacity,
        "effectiveOpacity": effective_opacity,
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
        SceneKind::Svg { source, .. } => {
            insert(details, "type", json!("svg"));
            insert(details, "source", json!(source));
        }
        SceneKind::Group { children, clip } => {
            insert(details, "type", json!("group"));
            insert(details, "clip", json!(clip));
            let children = describe_nodes(
                children,
                layout.world,
                layout.children_visible_area(*clip, visible_area),
                effective_opacity,
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
        report["width"],
        report["height"]
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
