//! Reads `tokens.json` into something to look at. Tokens are free-form JSON, so what each one
//! is gets decided by the shape of its value.

use std::sync::Arc;

use anyhow::Result;
use forma::diagnostics::Failure;
use forma::project::Project;
use forma::scene::{self, Resources};
use forma::{document, renderer};
use gpui_kit::{Hsla, RenderImage, Rgba};
use serde_json::{Value, json};

use crate::frame_render::to_render_image;

/// Previews are drawn by forma itself, so they match what frames will really produce.
const PREVIEW_SCALE: f64 = 2.0;
const GRADIENT_PREVIEW_WIDTH: f64 = 520.;
const GRADIENT_PREVIEW_HEIGHT: f64 = 208.;
const TYPE_SPECIMEN: &str = "Design as code";
/// Wide enough for the specimen in any ordinary typeface.
const TYPE_PREVIEW_EMS: f64 = 10.;

pub struct TokenPreview {
    pub image: Arc<RenderImage>,
    /// In design units.
    pub width: f32,
    pub height: f32,
}

pub enum TokenKind {
    Color {
        color: Hsla,
        text: String,
    },
    /// The error says why forma could not draw it, such as a missing font.
    Gradient(Result<TokenPreview, String>),
    Typography {
        preview: Result<TokenPreview, String>,
        summary: String,
    },
    Number(f64),
    Other(String),
}

pub struct Token {
    /// The name inside its group, such as `primary`.
    pub name: String,
    /// What a frame writes to use it, such as `$colors.primary`.
    pub reference: String,
    pub kind: TokenKind,
}

pub struct TokenGroup {
    pub title: String,
    pub tokens: Vec<Token>,
}

pub struct TokenSheet {
    pub groups: Vec<TokenGroup>,
}

impl TokenSheet {
    pub fn token_count(&self) -> usize {
        self.groups.iter().map(|group| group.tokens.len()).sum()
    }

    pub fn into_images(self) -> impl Iterator<Item = Arc<RenderImage>> {
        self.groups
            .into_iter()
            .flat_map(|group| group.tokens)
            .filter_map(|token| match token.kind {
                TokenKind::Gradient(preview) | TokenKind::Typography { preview, .. } => {
                    preview.ok().map(|preview| preview.image)
                }
                _ => None,
            })
    }
}

pub fn load(project: &Project) -> Result<TokenSheet> {
    let resources = project.resources()?;
    let tokens = resources.tokens.resolved()?;
    let groups = tokens
        .as_object()
        .into_iter()
        .flatten()
        .map(|(group_name, value)| {
            let mut leaves = Vec::new();
            collect_leaves(value, &mut vec![group_name.as_str()], &mut leaves);
            TokenGroup {
                title: title_case(group_name),
                tokens: leaves
                    .into_iter()
                    .map(|(path, value)| describe_token(&path, value, &resources))
                    .collect(),
            }
        })
        .collect();
    Ok(TokenSheet { groups })
}

fn title_case(name: &str) -> String {
    let mut characters = name.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// Groups nest freely, but gradients and type styles are single tokens despite being objects.
fn collect_leaves<'a>(
    value: &'a Value,
    path: &mut Vec<&'a str>,
    leaves: &mut Vec<(Vec<&'a str>, &'a Value)>,
) {
    match value.as_object() {
        Some(group) if !is_gradient(value) && !is_typography(value) => {
            for (key, child) in group {
                path.push(key);
                collect_leaves(child, path, leaves);
                path.pop();
            }
        }
        _ => leaves.push((path.clone(), value)),
    }
}

fn is_gradient(value: &Value) -> bool {
    matches!(
        value["type"].as_str(),
        Some("linear-gradient" | "radial-gradient")
    )
}

fn is_typography(value: &Value) -> bool {
    value["family"].is_string() && value["size"].is_number()
}

fn describe_token(path: &[&str], value: &Value, resources: &Resources) -> Token {
    let reference = format!("${}", path.join("."));
    let kind = match value {
        Value::Number(number) => TokenKind::Number(number.as_f64().unwrap_or_default()),
        Value::String(text) => match Rgba::try_from(text.as_str()) {
            Ok(color) => TokenKind::Color {
                color: color.into(),
                text: text.clone(),
            },
            Err(_) => TokenKind::Other(text.clone()),
        },
        _ if is_gradient(value) => TokenKind::Gradient(preview_gradient(&reference, resources)),
        _ if is_typography(value) => TokenKind::Typography {
            preview: preview_typography(&reference, value, resources),
            summary: typography_summary(value),
        },
        other => TokenKind::Other(other.to_string()),
    };
    Token {
        name: path[1..].join("."),
        reference,
        kind,
    }
}

fn typography_summary(style: &Value) -> String {
    let mut parts = vec![style["family"].as_str().unwrap_or_default().to_owned()];
    for key in ["size", "weight", "lineHeight"] {
        if !style[key].is_null() {
            parts.push(style[key].to_string());
        }
    }
    parts.join(" · ")
}

fn preview_gradient(reference: &str, resources: &Resources) -> Result<TokenPreview, String> {
    render_preview(
        &json!({
            "version": 1,
            "id": "preview",
            "width": GRADIENT_PREVIEW_WIDTH,
            "height": GRADIENT_PREVIEW_HEIGHT,
            "background": reference,
            "children": [],
        }),
        resources,
    )
}

fn preview_typography(
    reference: &str,
    style: &Value,
    resources: &Resources,
) -> Result<TokenPreview, String> {
    let size = style["size"].as_f64().unwrap_or_default();
    let line_height = style["lineHeight"].as_f64().unwrap_or(1.2);
    let width = (size * TYPE_PREVIEW_EMS).ceil();
    render_preview(
        &json!({
            "version": 1,
            "id": "preview",
            "width": width,
            "height": (size * line_height.max(1.2)).ceil(),
            "children": [{
                "id": "specimen",
                "type": "text",
                "text": TYPE_SPECIMEN,
                "width": width,
                "font": reference,
                "fill": { "color": "#FFFFFF" },
            }],
        }),
        resources,
    )
}

fn render_preview(frame: &Value, resources: &Resources) -> Result<TokenPreview, String> {
    let render = || -> Result<TokenPreview, Failure> {
        let frame = document::parse_frame(&frame.to_string())?;
        let scene = scene::build(&frame, resources)?;
        let canvas = renderer::render(&scene, PREVIEW_SCALE)?;
        Ok(TokenPreview {
            image: to_render_image(&canvas),
            width: scene.width as f32,
            height: scene.height as f32,
        })
    };
    render().map_err(|failure| {
        failure
            .0
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    })
}

#[cfg(test)]
mod tests {
    use forma::assets::Assets;
    use forma::text::FontLibrary;
    use forma::tokens::Tokens;

    use super::*;

    fn resources(tokens: &str) -> Resources {
        Resources {
            tokens: Tokens::parse(tokens).unwrap(),
            assets: Assets::new("/no/assets".into()),
            fonts: FontLibrary::default(),
        }
    }

    fn kind_of(tokens: &str, reference: &str) -> TokenKind {
        let resources = resources(tokens);
        let root = resources.tokens.resolved().unwrap();
        let path: Vec<&str> = reference.trim_start_matches('$').split('.').collect();
        let value = path.iter().fold(&root, |parent, key| &parent[key]);
        describe_token(&path, value, &resources).kind
    }

    #[test]
    fn hex_strings_are_colors_and_other_strings_are_not() {
        let tokens = r##"{ "colors": { "primary": "#FF0000" }, "misc": { "note": "hello" } }"##;

        assert!(matches!(
            kind_of(tokens, "$colors.primary"),
            TokenKind::Color { .. }
        ));
        assert!(matches!(
            kind_of(tokens, "$misc.note"),
            TokenKind::Other(text) if text == "hello"
        ));
    }

    #[test]
    fn numbers_are_numbers() {
        assert!(matches!(
            kind_of(r#"{ "spacing": { "md": 32 } }"#, "$spacing.md"),
            TokenKind::Number(32.0)
        ));
    }

    #[test]
    fn gradients_are_drawn_by_forma() {
        let tokens = r##"{ "gradients": { "brand": { "type": "linear-gradient", "angle": 90,
            "stops": [{ "offset": 0, "color": "#FF0000" }, { "offset": 1, "color": "#0000FF" }] } } }"##;

        let TokenKind::Gradient(Ok(preview)) = kind_of(tokens, "$gradients.brand") else {
            panic!("the gradient should have a preview");
        };

        assert_eq!(preview.width, GRADIENT_PREVIEW_WIDTH as f32);
    }

    #[test]
    fn a_missing_font_is_reported_on_the_token_and_keeps_its_summary() {
        let tokens = r#"{ "typography": { "heading": {
            "family": "No Such Family", "size": 96, "weight": 800, "lineHeight": 1.05 } } }"#;

        let TokenKind::Typography { preview, summary } = kind_of(tokens, "$typography.heading")
        else {
            panic!("the style should be typography");
        };

        assert!(preview.is_err());
        assert_eq!(summary, "No Such Family · 96 · 800 · 1.05");
    }

    #[test]
    fn nested_groups_flatten_into_dotted_names() {
        let value = json!({ "brand": { "500": "#FF0000" }, "plain": "#00FF00" });
        let mut leaves = Vec::new();

        collect_leaves(&value, &mut vec!["colors"], &mut leaves);

        let paths: Vec<_> = leaves.iter().map(|(path, _)| path.join(".")).collect();
        assert_eq!(paths, ["colors.brand.500", "colors.plain"]);
    }

    #[test]
    fn group_titles_are_capitalized() {
        assert_eq!(title_case("typography"), "Typography");
    }
}
