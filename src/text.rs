//! Font selection and text layout. Fonts are never substituted without being declared.

use std::cell::OnceCell;

use fontdb::{Database, Family, Query, Stretch, Style, Weight};
use rustybuzz::ttf_parser::{GlyphId, OutlineBuilder};
use rustybuzz::{Direction, Face, UnicodeBuffer};
use tiny_skia::{Path, PathBuilder};

use crate::assets::Assets;
use crate::diagnostics::Diagnostic;
use crate::document::TextAlign;
use crate::tokens::ResolvedFont;

#[derive(Debug)]
pub struct TextLayout {
    /// Glyph outlines in the text box's coordinates; `None` for blank text.
    pub outlines: Option<Path>,
    pub width: f64,
    pub height: f64,
    pub line_count: usize,
    /// The font actually used: a file path or a family name.
    pub font_source: String,
    pub warnings: Vec<Diagnostic>,
}

struct LoadedFont {
    data: Vec<u8>,
    face_index: u32,
    source: String,
    is_declared_fallback: bool,
    warning: Option<Diagnostic>,
}

/// What to lay out, and where it came from for diagnostics.
pub struct TextRequest<'a> {
    pub text: &'a str,
    pub font: &'a ResolvedFont,
    pub wrap_width: Option<f64>,
    pub align: TextAlign,
    pub path: &'a str,
}

#[derive(Debug, Default)]
pub struct FontLibrary {
    /// Scanning system fonts is slow, and frames using only project fonts never need it.
    system_fonts: OnceCell<Database>,
}

impl FontLibrary {
    fn system_fonts(&self) -> &Database {
        self.system_fonts.get_or_init(|| {
            let mut database = Database::new();
            database.load_system_fonts();
            database
        })
    }

    fn load_system_font(
        &self,
        family: &str,
        font: &ResolvedFont,
        path: &str,
    ) -> Option<LoadedFont> {
        let database = self.system_fonts();
        let style = if font.italic {
            Style::Italic
        } else {
            Style::Normal
        };
        let id = database.query(&Query {
            families: &[Family::Name(family)],
            weight: Weight(font.weight),
            stretch: Stretch::Normal,
            style,
        })?;
        let face = database.face(id)?;
        let warning = (face.weight != Weight(font.weight) || face.style != style).then(|| {
            Diagnostic::new(
                "font_style_substituted",
                format!(
                    "\"{family}\" has no exact match for weight {}; using weight {} style {:?}",
                    font.weight, face.weight.0, face.style
                ),
            )
            .at(path)
        });
        let (data, face_index) =
            database.with_face_data(id, |data, face_index| (data.to_vec(), face_index))?;
        Some(LoadedFont {
            data,
            face_index,
            source: family.to_owned(),
            is_declared_fallback: false,
            warning,
        })
    }

    /// Declared fonts in priority order. A missing primary is only allowed with declared fallbacks.
    fn candidates(
        &self,
        font: &ResolvedFont,
        assets: &Assets,
        path: &str,
    ) -> Result<Vec<LoadedFont>, Diagnostic> {
        let mut candidates = Vec::new();
        if let Some(file) = &font.file {
            let (resolved, data) = assets.read(file, &format!("{path}.file"))?;
            candidates.push(LoadedFont {
                data,
                face_index: 0,
                source: resolved.display().to_string(),
                is_declared_fallback: false,
                warning: None,
            });
        } else if let Some(family) = &font.family {
            candidates.extend(self.load_system_font(family, font, path));
        }
        for family in &font.fallbacks {
            let fallback = self.load_system_font(family, font, path);
            candidates.extend(fallback.map(|fallback| LoadedFont {
                is_declared_fallback: true,
                ..fallback
            }));
        }
        if candidates.is_empty() {
            let name = font.family.as_deref().unwrap_or("(no family)");
            return Err(Diagnostic::new(
                "font_not_found",
                format!("font \"{name}\" is not installed; add a `file` under assets/fonts"),
            )
            .at(path));
        }
        Ok(candidates)
    }

    pub fn layout(&self, request: &TextRequest, assets: &Assets) -> Result<TextLayout, Diagnostic> {
        let TextRequest {
            text, font, path, ..
        } = *request;
        for candidate in self.candidates(font, assets, path)? {
            let face =
                Face::from_slice(&candidate.data, candidate.face_index).ok_or_else(|| {
                    Diagnostic::new(
                        "invalid_font",
                        format!("{} is not a usable font", candidate.source),
                    )
                    .at(path)
                })?;
            if !covers_text(&face, text) {
                continue;
            }
            let mut layout = lay_out(&face, request);
            layout.warnings.extend(candidate.warning);
            if candidate.is_declared_fallback {
                layout.warnings.push(
                    Diagnostic::new(
                        "font_fallback_used",
                        format!("using declared fallback \"{}\"", candidate.source),
                    )
                    .at(path),
                );
            }
            layout.font_source = candidate.source;
            return Ok(layout);
        }
        Err(Diagnostic::new(
            "missing_glyphs",
            "no declared font covers every character of this text",
        )
        .at(path))
    }
}

fn covers_text(face: &Face, text: &str) -> bool {
    text.chars()
        .filter(|character| !character.is_whitespace() && !character.is_control())
        .all(|character| face.glyph_index(character).is_some())
}

struct ShapedGlyph {
    glyph_id: u16,
    x_offset: f64,
    y_offset: f64,
    advance: f64,
}

struct ShapedLine {
    glyphs: Vec<ShapedGlyph>,
    width: f64,
}

fn shape_line(face: &Face, line: &str, font: &ResolvedFont, units_to_pixels: f64) -> ShapedLine {
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(line);
    buffer.guess_segment_properties();
    // V1 lays out left to right only.
    buffer.set_direction(Direction::LeftToRight);
    let shaped = rustybuzz::shape(face, &[], buffer);
    let glyphs: Vec<_> = shaped
        .glyph_infos()
        .iter()
        .zip(shaped.glyph_positions())
        .map(|(info, position)| ShapedGlyph {
            glyph_id: info.glyph_id as u16,
            x_offset: f64::from(position.x_offset) * units_to_pixels,
            y_offset: f64::from(position.y_offset) * units_to_pixels,
            advance: f64::from(position.x_advance) * units_to_pixels + font.letter_spacing,
        })
        .collect();
    let width = glyphs.iter().map(|glyph| glyph.advance).sum();
    ShapedLine { glyphs, width }
}

/// Greedy wrapping, re-measuring each candidate line so kerning is accounted for.
fn wrap_paragraph(paragraph: &str, wrap_width: f64, measure: impl Fn(&str) -> f64) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current_line = String::new();
    for word in paragraph.split(' ') {
        if current_line.is_empty() {
            current_line.push_str(word);
            continue;
        }
        let candidate = format!("{current_line} {word}");
        if measure(&candidate) <= wrap_width {
            current_line = candidate;
        } else {
            lines.push(std::mem::replace(&mut current_line, word.to_owned()));
        }
    }
    lines.push(current_line);
    lines
}

fn lay_out(face: &Face, request: &TextRequest) -> TextLayout {
    let TextRequest {
        text,
        font,
        wrap_width,
        align,
        path,
    } = *request;
    let units_to_pixels = font.size / f64::from(face.units_per_em());
    let line_strings: Vec<String> = text
        .split('\n')
        .flat_map(|paragraph| match wrap_width {
            Some(width) => wrap_paragraph(paragraph, width, |line| {
                shape_line(face, line, font, units_to_pixels).width
            }),
            None => vec![paragraph.to_owned()],
        })
        .collect();
    let lines: Vec<ShapedLine> = line_strings
        .iter()
        .map(|line| shape_line(face, line, font, units_to_pixels))
        .collect();

    let widest_line = lines.iter().map(|line| line.width).fold(0.0, f64::max);
    let box_width = wrap_width.unwrap_or(widest_line);
    let line_height = font.size * font.line_height;
    let ascent = f64::from(face.ascender()) * units_to_pixels;
    let descent = f64::from(face.descender()) * units_to_pixels;
    let baseline_within_line = (line_height - (ascent - descent)) / 2.0 + ascent;

    let mut builder = PathBuilder::new();
    for (line_index, line) in lines.iter().enumerate() {
        let mut pen_x = match align {
            TextAlign::Left => 0.0,
            TextAlign::Center => (box_width - line.width) / 2.0,
            TextAlign::Right => box_width - line.width,
        };
        let baseline = line_index as f64 * line_height + baseline_within_line;
        for glyph in &line.glyphs {
            let mut sink = GlyphOutlineSink {
                builder: &mut builder,
                origin_x: pen_x + glyph.x_offset,
                origin_y: baseline - glyph.y_offset,
                units_to_pixels,
            };
            face.outline_glyph(GlyphId(glyph.glyph_id), &mut sink);
            pen_x += glyph.advance;
        }
    }

    let mut warnings = Vec::new();
    if wrap_width.is_some_and(|width| widest_line > width) {
        warnings
            .push(Diagnostic::new("text_overflow", "a word is wider than the text box").at(path));
    }
    TextLayout {
        outlines: builder.finish(),
        width: box_width,
        height: lines.len() as f64 * line_height,
        line_count: lines.len(),
        font_source: String::new(),
        warnings,
    }
}

/// Converts font units (Y up) into text-box pixels (Y down).
struct GlyphOutlineSink<'a> {
    builder: &'a mut PathBuilder,
    origin_x: f64,
    origin_y: f64,
    units_to_pixels: f64,
}

impl GlyphOutlineSink<'_> {
    fn x(&self, x: f32) -> f32 {
        (self.origin_x + f64::from(x) * self.units_to_pixels) as f32
    }

    fn y(&self, y: f32) -> f32 {
        (self.origin_y - f64::from(y) * self.units_to_pixels) as f32
    }
}

impl OutlineBuilder for GlyphOutlineSink<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.builder.move_to(self.x(x), self.y(y));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.builder.line_to(self.x(x), self.y(y));
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (x1, y1, x, y) = (self.x(x1), self.y(y1), self.x(x), self.y(y));
        self.builder.quad_to(x1, y1, x, y);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (x1, y1) = (self.x(x1), self.y(y1));
        let (x2, y2, x, y) = (self.x(x2), self.y(y2), self.x(x), self.y(y));
        self.builder.cubic_to(x1, y1, x2, y2, x, y);
    }

    fn close(&mut self) {
        self.builder.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_assets() -> Assets {
        Assets::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures").into())
    }

    fn fixture_font() -> ResolvedFont {
        ResolvedFont {
            family: None,
            file: Some("Tuffy.ttf".to_owned()),
            size: 40.0,
            weight: 400,
            italic: false,
            line_height: 1.5,
            letter_spacing: 0.0,
            fallbacks: Vec::new(),
        }
    }

    fn layout(
        text: &str,
        font: &ResolvedFont,
        wrap_width: Option<f64>,
    ) -> Result<TextLayout, Diagnostic> {
        let request = TextRequest {
            text,
            font,
            wrap_width,
            align: TextAlign::Left,
            path: "font",
        };
        FontLibrary::default().layout(&request, &fixture_assets())
    }

    #[test]
    fn text_wraps_at_the_box_width_and_at_explicit_newlines() {
        let font = fixture_font();
        let unwrapped = layout("design as code", &font, None).unwrap();
        assert_eq!(unwrapped.line_count, 1);

        let wrapped = layout("design as code", &font, Some(unwrapped.width * 0.75)).unwrap();
        assert_eq!(wrapped.line_count, 2);
        assert_eq!(wrapped.height, 2.0 * 60.0);
        assert!(wrapped.warnings.is_empty());

        assert_eq!(layout("one\ntwo\n", &font, None).unwrap().line_count, 3);

        let overflowing = layout("unbreakable", &font, Some(10.0)).unwrap();
        assert_eq!(overflowing.warnings[0].code, "text_overflow");
    }

    #[test]
    fn missing_fonts_and_glyphs_are_errors_rather_than_substitutions() {
        let absent_family = ResolvedFont {
            family: Some("No Such Family Anywhere".to_owned()),
            file: None,
            ..fixture_font()
        };
        let error = layout("text", &absent_family, None).unwrap_err();
        assert_eq!(error.code, "font_not_found");

        let error = layout("日本語", &fixture_font(), None).unwrap_err();
        assert_eq!(error.code, "missing_glyphs");
    }
}
