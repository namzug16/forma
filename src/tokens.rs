//! Typed design tokens, plus the resolved values every `$reference` turns into.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;

use crate::diagnostics::{Diagnostic, Failure};
use crate::document::{self, Fill, FontStyle, FontValue, Gradient, GradientStop, NumberValue};

const DEFAULT_FONT_WEIGHT: u16 = 400;
const DEFAULT_LINE_HEIGHT: f64 = 1.2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl Color {
    /// Parses `#RGB`, `#RGBA`, `#RRGGBB` and `#RRGGBBAA`.
    pub fn from_hex(text: &str) -> Option<Self> {
        let digits = text.strip_prefix('#')?;
        if !digits.is_ascii() {
            return None;
        }
        let channel_width = if digits.len() <= 4 { 1 } else { 2 };
        let channels = digits
            .as_bytes()
            .chunks(channel_width)
            .map(|chunk| {
                let value = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
                Some(if channel_width == 1 {
                    value * 17
                } else {
                    value
                })
            })
            .collect::<Option<Vec<u8>>>()?;
        match (digits.len(), channels.as_slice()) {
            (3 | 6, &[red, green, blue]) => Some(Self {
                red,
                green,
                blue,
                alpha: 255,
            }),
            (4 | 8, &[red, green, blue, alpha]) => Some(Self {
                red,
                green,
                blue,
                alpha,
            }),
            _ => None,
        }
    }
}

impl Serialize for Color {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let Self {
            red,
            green,
            blue,
            alpha,
        } = self;
        serializer.collect_str(&format_args!("#{red:02X}{green:02X}{blue:02X}{alpha:02X}"))
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ColorStop {
    pub offset: f64,
    pub color: Color,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Paint {
    Solid {
        color: Color,
    },
    LinearGradient {
        angle: f64,
        stops: Vec<ColorStop>,
    },
    RadialGradient {
        center: [f64; 2],
        radius: f64,
        stops: Vec<ColorStop>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedFont {
    pub family: Option<String>,
    pub file: Option<String>,
    pub size: f64,
    pub weight: u16,
    pub italic: bool,
    pub line_height: f64,
    pub letter_spacing: f64,
    pub fallbacks: Vec<String>,
}

#[derive(Debug)]
pub struct Tokens {
    root: Value,
}

impl Tokens {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("cannot read tokens {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("invalid tokens {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let root: Value = serde_json::from_str(text)?;
        anyhow::ensure!(root.is_object(), "tokens must be a JSON object");
        Ok(Self { root })
    }

    /// Follows a chain of references down to a concrete value.
    fn lookup<'a>(&'a self, reference: &'a str, path: &str) -> Result<&'a Value, Diagnostic> {
        let mut visited = Vec::new();
        let mut current = reference;
        loop {
            if visited.contains(&current) {
                visited.push(current);
                return Err(Diagnostic::new(
                    "circular_token_reference",
                    format!("circular token reference: {}", visited.join(" -> ")),
                )
                .at(path));
            }
            visited.push(current);
            let value = current
                .strip_prefix('$')
                .and_then(|name| {
                    name.split('.')
                        .try_fold(&self.root, |parent, key| parent.get(key))
                })
                .ok_or_else(|| {
                    Diagnostic::new(
                        "unresolved_token",
                        format!("token {current} is not defined in tokens.json"),
                    )
                    .at(path)
                })?;
            match value {
                Value::String(next) if next.starts_with('$') => current = next,
                concrete => return Ok(concrete),
            }
        }
    }

    pub fn number(&self, value: &NumberValue, path: &str) -> Result<f64, Diagnostic> {
        match value {
            NumberValue::Literal(number) => Ok(*number),
            NumberValue::Token(reference) => self
                .lookup(reference, path)?
                .as_f64()
                .ok_or_else(|| type_mismatch(reference, "a number", path)),
        }
    }

    pub fn color(&self, text: &str, path: &str) -> Result<Color, Diagnostic> {
        let hex = if text.starts_with('$') {
            self.lookup(text, path)?
                .as_str()
                .ok_or_else(|| type_mismatch(text, "a color", path))?
        } else {
            text
        };
        Color::from_hex(hex).ok_or_else(|| {
            Diagnostic::new(
                "invalid_color",
                format!("\"{hex}\" is not #RGB, #RGBA, #RRGGBB or #RRGGBBAA"),
            )
            .at(path)
        })
    }

    pub fn paint(&self, fill: &Fill, path: &str) -> Result<Paint, Diagnostic> {
        match fill {
            Fill::Solid(solid) => Ok(Paint::Solid {
                color: self.color(&solid.color, &format!("{path}.color"))?,
            }),
            Fill::Gradient(Gradient::LinearGradient(gradient)) => Ok(Paint::LinearGradient {
                angle: gradient.angle,
                stops: self.stops(&gradient.stops, path)?,
            }),
            Fill::Gradient(Gradient::RadialGradient(gradient)) => {
                if gradient.radius <= 0.0 {
                    return Err(Diagnostic::new("invalid_value", "radius must be positive")
                        .at(format!("{path}.radius")));
                }
                Ok(Paint::RadialGradient {
                    center: gradient.center,
                    radius: gradient.radius,
                    stops: self.stops(&gradient.stops, path)?,
                })
            }
            Fill::Token(reference) => match self.lookup(reference, path)? {
                Value::String(_) => Ok(Paint::Solid {
                    color: self.color(reference, path)?,
                }),
                value => {
                    let gradient = serde_json::from_value::<Gradient>(value.clone())
                        .map_err(|_| type_mismatch(reference, "a color or gradient", path))?;
                    self.paint(&Fill::Gradient(gradient), path)
                }
            },
        }
    }

    fn stops(&self, stops: &[GradientStop], path: &str) -> Result<Vec<ColorStop>, Diagnostic> {
        if stops.len() < 2 {
            return Err(Diagnostic::new(
                "invalid_gradient_stop",
                "a gradient needs at least two stops",
            )
            .at(format!("{path}.stops")));
        }
        let mut previous_offset = 0.0;
        stops
            .iter()
            .enumerate()
            .map(|(index, stop)| {
                let stop_path = format!("{path}.stops[{index}]");
                if !(previous_offset..=1.0).contains(&stop.offset) {
                    return Err(Diagnostic::new(
                        "invalid_gradient_stop",
                        "offsets must be within [0, 1] and nondecreasing",
                    )
                    .at(format!("{stop_path}.offset")));
                }
                previous_offset = stop.offset;
                Ok(ColorStop {
                    offset: stop.offset,
                    color: self.color(&stop.color, &format!("{stop_path}.color"))?,
                })
            })
            .collect()
    }

    pub fn font(&self, value: &FontValue, path: &str) -> Result<ResolvedFont, Diagnostic> {
        let font = match value {
            FontValue::Inline(font) => font.clone(),
            FontValue::Token(reference) => {
                let value = self.lookup(reference, path)?;
                serde_json::from_value::<document::Font>(value.clone())
                    .map_err(|_| type_mismatch(reference, "a typography object", path))?
            }
        };
        let size = self.number(&font.size, &format!("{path}.size"))?;
        if size <= 0.0 {
            return Err(
                Diagnostic::new("invalid_value", "font size must be positive")
                    .at(format!("{path}.size")),
            );
        }
        if font.family.is_none() && font.file.is_none() {
            return Err(
                Diagnostic::new("invalid_value", "a font needs a `family` or a `file`").at(path),
            );
        }
        Ok(ResolvedFont {
            family: font.family,
            file: font.file,
            size,
            weight: font.weight.unwrap_or(DEFAULT_FONT_WEIGHT),
            italic: matches!(font.style.unwrap_or_default(), FontStyle::Italic),
            line_height: font.line_height.unwrap_or(DEFAULT_LINE_HEIGHT),
            letter_spacing: font.letter_spacing.unwrap_or(0.0),
            fallbacks: font.fallbacks.unwrap_or_default(),
        })
    }

    /// Every token with references replaced by values; reports all failures.
    pub fn resolved(&self) -> Result<Value, Failure> {
        let mut diagnostics = Vec::new();
        let resolved = self.resolve_tree(&self.root, "", &mut Vec::new(), &mut diagnostics);
        if diagnostics.is_empty() {
            Ok(resolved)
        } else {
            Err(Failure(diagnostics))
        }
    }

    fn resolve_tree<'a>(
        &'a self,
        value: &'a Value,
        path: &str,
        active_references: &mut Vec<&'a str>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Value {
        match value {
            Value::Object(entries) => Value::Object(
                entries
                    .iter()
                    .map(|(key, entry)| {
                        let entry_path = if path.is_empty() {
                            key.clone()
                        } else {
                            format!("{path}.{key}")
                        };
                        let resolved =
                            self.resolve_tree(entry, &entry_path, active_references, diagnostics);
                        (key.clone(), resolved)
                    })
                    .collect(),
            ),
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        let item_path = format!("{path}[{index}]");
                        self.resolve_tree(item, &item_path, active_references, diagnostics)
                    })
                    .collect(),
            ),
            Value::String(text) if text.starts_with('$') => {
                if active_references.contains(&text.as_str()) {
                    diagnostics.push(
                        Diagnostic::new(
                            "circular_token_reference",
                            format!("{text} contains a reference to itself"),
                        )
                        .at(path),
                    );
                    return Value::Null;
                }
                match self.lookup(text, path) {
                    Ok(target) => {
                        active_references.push(text);
                        let resolved =
                            self.resolve_tree(target, path, active_references, diagnostics);
                        active_references.pop();
                        resolved
                    }
                    Err(diagnostic) => {
                        diagnostics.push(diagnostic);
                        Value::Null
                    }
                }
            }
            concrete => concrete.clone(),
        }
    }
}

fn type_mismatch(reference: &str, expected: &str, path: &str) -> Diagnostic {
    Diagnostic::new(
        "token_type_mismatch",
        format!("token {reference} is not {expected}"),
    )
    .at(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circular_references_fail_instead_of_recursing_forever() {
        let tokens = Tokens::parse(
            r#"{
                "colors": { "a": "$colors.b", "b": "$colors.a" },
                "typography": { "self": { "family": "X", "size": "$typography.self" } }
            }"#,
        )
        .unwrap();

        let chain = tokens.color("$colors.a", "fill.color").unwrap_err();
        assert_eq!(chain.code, "circular_token_reference");
        assert_eq!(chain.path.as_deref(), Some("fill.color"));

        let failure = tokens.resolved().unwrap_err();
        let codes: Vec<_> = failure.0.iter().map(|d| d.code).collect();
        assert_eq!(
            codes
                .iter()
                .filter(|c| **c == "circular_token_reference")
                .count(),
            3
        );
    }

    #[test]
    fn a_token_of_the_wrong_type_is_rejected_at_its_destination() {
        let tokens =
            Tokens::parse(r##"{ "colors": { "main": "#FFF" }, "spacing": { "lg": 64 } }"##)
                .unwrap();

        let number = tokens.number(&NumberValue::Token("$colors.main".into()), "x");
        assert_eq!(number.unwrap_err().code, "token_type_mismatch");

        let color = tokens.color("$spacing.lg", "fill.color");
        assert_eq!(color.unwrap_err().code, "token_type_mismatch");
    }
}
