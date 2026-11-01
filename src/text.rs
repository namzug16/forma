//! Font selection and text layout. Fonts are never substituted without being declared.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::sync::Arc;

use fontdb::{Database, Family, ID, Query, Source, Stretch, Style, Weight};
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

/// One family's matching files. Distributions like Fontsource split a font by character subset,
/// so several files can together form one font; the first sets the line metrics.
struct LoadedFont {
    files: Vec<FontFile>,
    source: String,
    is_declared_fallback: bool,
    warning: Option<Diagnostic>,
}

struct FontFile {
    data: Vec<u8>,
    face_index: u32,
}

/// What to lay out, and where it came from for diagnostics.
pub struct TextRequest<'a> {
    pub text: &'a str,
    pub font: &'a ResolvedFont,
    pub wrap_width: Option<f64>,
    pub align: TextAlign,
    pub path: &'a str,
}

/// Everything under `assets/fonts`, loaded through `Assets` so its path rules apply.
#[derive(Debug, Default)]
pub struct ProjectFonts {
    pub database: Database,
    /// Absolute paths, for reporting which file a face came from.
    pub paths: HashMap<ID, String>,
}

impl ProjectFonts {
    fn load(assets: &Assets) -> Result<Self, Diagnostic> {
        let mut fonts = Self::default();
        for file in assets.font_files()? {
            let (resolved, data) = assets.read(&file, &file)?;
            let path = resolved.display().to_string();
            let ids = fonts
                .database
                .load_font_source(Source::Binary(Arc::new(data)));
            if ids.is_empty() {
                return Err(Diagnostic::new(
                    "invalid_font",
                    format!("{path} is not a usable font"),
                ));
            }
            fonts
                .paths
                .extend(ids.into_iter().map(|id| (id, path.clone())));
        }
        Ok(fonts)
    }
}

#[derive(Debug, Default)]
pub struct FontLibrary {
    /// Reading fonts is slow, and frames that name a font file need neither of these.
    project_fonts: OnceCell<Result<ProjectFonts, Diagnostic>>,
    system_fonts: OnceCell<Database>,
}

impl FontLibrary {
    pub fn project_fonts(&self, assets: &Assets) -> Result<&ProjectFonts, Diagnostic> {
        self.project_fonts
            .get_or_init(|| ProjectFonts::load(assets))
            .as_ref()
            .map_err(Clone::clone)
    }

    pub fn system_fonts(&self) -> &Database {
        self.system_fonts.get_or_init(|| {
            let mut database = Database::new();
            database.load_system_fonts();
            database
        })
    }

    /// Project fonts win, so a checked-in font renders the same on every machine.
    fn load_family(
        &self,
        family: &str,
        font: &ResolvedFont,
        assets: &Assets,
        path: &str,
    ) -> Result<Option<LoadedFont>, Diagnostic> {
        let project = self.project_fonts(assets)?;
        let from_project = load_from_database(&project.database, family, font, path, |id| {
            project.paths[&id].clone()
        });
        Ok(from_project.or_else(|| {
            load_from_database(self.system_fonts(), family, font, path, |_| {
                family.to_owned()
            })
        }))
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
                files: vec![FontFile {
                    data,
                    face_index: 0,
                }],
                source: resolved.display().to_string(),
                is_declared_fallback: false,
                warning: None,
            });
        } else if let Some(family) = &font.family {
            candidates.extend(self.load_family(family, font, assets, path)?);
        }
        for family in &font.fallbacks {
            let fallback = self.load_family(family, font, assets, path)?;
            candidates.extend(fallback.map(|fallback| LoadedFont {
                is_declared_fallback: true,
                ..fallback
            }));
        }
        if candidates.is_empty() {
            let name = font.family.as_deref().unwrap_or("(no family)");
            return Err(Diagnostic::new(
                "font_not_found",
                format!("font \"{name}\" is not installed; run `forma fonts add \"{name}\"`"),
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
            let faces = candidate
                .files
                .iter()
                .map(|file| {
                    Face::from_slice(&file.data, file.face_index).ok_or_else(|| {
                        Diagnostic::new(
                            "invalid_font",
                            format!("{} is not a usable font", candidate.source),
                        )
                        .at(path)
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            if !covers_text(&faces, text) {
                continue;
            }
            let mut layout = lay_out(&faces, request);
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

fn load_from_database(
    database: &Database,
    family: &str,
    font: &ResolvedFont,
    path: &str,
    source_of: impl Fn(ID) -> String,
) -> Option<LoadedFont> {
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
    let siblings = database
        .faces()
        .filter(|other| {
            other.id != id
                && other.weight == face.weight
                && other.style == face.style
                && other.stretch == face.stretch
                && other
                    .families
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(family))
        })
        .map(|other| other.id);
    let files = std::iter::once(id)
        .chain(siblings)
        .map(|id| {
            database.with_face_data(id, |data, face_index| FontFile {
                data: data.to_vec(),
                face_index,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(LoadedFont {
        files,
        source: source_of(id),
        is_declared_fallback: false,
        warning,
    })
}

/// A base character with its combining marks, which must stay together to compose.
fn clusters(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut starts = text
        .char_indices()
        .filter(|(_, character)| !matches!(character, '\u{0300}'..='\u{036F}'))
        .map(|(index, _)| index)
        .peekable();
    std::iter::from_fn(move || {
        let start = starts.next()?;
        let end = starts.peek().copied().unwrap_or(text.len());
        Some((start, &text[start..end]))
    })
}

fn needs_no_glyph(cluster: &str) -> bool {
    cluster
        .chars()
        .all(|character| character.is_whitespace() || character.is_control())
}

/// Shaping decides for marks, because a font can compose them into a single glyph it does have.
fn face_covers(face: &Face, cluster: &str) -> bool {
    let mut characters = cluster.chars();
    if let (Some(character), None) = (characters.next(), characters.next()) {
        return face.glyph_index(character).is_some();
    }
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(cluster);
    buffer.guess_segment_properties();
    let shaped = rustybuzz::shape(face, &[], buffer);
    shaped.glyph_infos().iter().all(|info| info.glyph_id != 0)
}

fn covers_text(faces: &[Face], text: &str) -> bool {
    clusters(text).all(|(_, cluster)| {
        needs_no_glyph(cluster) || faces.iter().any(|face| face_covers(face, cluster))
    })
}

struct ShapedGlyph {
    face_index: usize,
    glyph_id: u16,
    x_offset: f64,
    y_offset: f64,
    advance: f64,
}

struct ShapedLine {
    glyphs: Vec<ShapedGlyph>,
    width: f64,
}

fn units_to_pixels(face: &Face, font: &ResolvedFont) -> f64 {
    font.size / f64::from(face.units_per_em())
}

/// Splits a line into runs of one font file, staying on the current file while it can.
fn file_runs<'a>(faces: &[Face], line: &'a str) -> Vec<(usize, &'a str)> {
    let mut runs = Vec::new();
    let (mut run_start, mut current_face) = (0, 0);
    for (offset, cluster) in clusters(line) {
        if needs_no_glyph(cluster) || face_covers(&faces[current_face], cluster) {
            continue;
        }
        let covering_face = faces.iter().position(|face| face_covers(face, cluster));
        let next_face = covering_face.unwrap_or(current_face);
        if next_face != current_face {
            if offset > run_start {
                runs.push((current_face, &line[run_start..offset]));
            }
            (run_start, current_face) = (offset, next_face);
        }
    }
    runs.push((current_face, &line[run_start..]));
    runs
}

fn shape_line(faces: &[Face], line: &str, font: &ResolvedFont) -> ShapedLine {
    let mut glyphs = Vec::new();
    for (face_index, run) in file_runs(faces, line) {
        let face = &faces[face_index];
        let scale = units_to_pixels(face, font);
        let mut buffer = UnicodeBuffer::new();
        buffer.push_str(run);
        buffer.guess_segment_properties();
        // V1 lays out left to right only.
        buffer.set_direction(Direction::LeftToRight);
        let shaped = rustybuzz::shape(face, &[], buffer);
        glyphs.extend(
            shaped
                .glyph_infos()
                .iter()
                .zip(shaped.glyph_positions())
                .map(|(info, position)| ShapedGlyph {
                    face_index,
                    glyph_id: info.glyph_id as u16,
                    x_offset: f64::from(position.x_offset) * scale,
                    y_offset: f64::from(position.y_offset) * scale,
                    advance: f64::from(position.x_advance) * scale + font.letter_spacing,
                }),
        );
    }
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

fn lay_out(faces: &[Face], request: &TextRequest) -> TextLayout {
    let TextRequest {
        text,
        font,
        wrap_width,
        align,
        path,
    } = *request;
    let line_strings: Vec<String> = text
        .split('\n')
        .flat_map(|paragraph| match wrap_width {
            Some(width) => {
                wrap_paragraph(paragraph, width, |line| shape_line(faces, line, font).width)
            }
            None => vec![paragraph.to_owned()],
        })
        .collect();
    let lines: Vec<ShapedLine> = line_strings
        .iter()
        .map(|line| shape_line(faces, line, font))
        .collect();

    let widest_line = lines.iter().map(|line| line.width).fold(0.0, f64::max);
    let box_width = wrap_width.unwrap_or(widest_line);
    let line_height = font.size * font.line_height;
    let metrics_face = &faces[0];
    let metrics_scale = units_to_pixels(metrics_face, font);
    let ascent = f64::from(metrics_face.ascender()) * metrics_scale;
    let descent = f64::from(metrics_face.descender()) * metrics_scale;
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
            let face = &faces[glyph.face_index];
            let mut sink = GlyphOutlineSink {
                builder: &mut builder,
                origin_x: pen_x + glyph.x_offset,
                origin_y: baseline - glyph.y_offset,
                units_to_pixels: units_to_pixels(face, font),
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
    fn a_project_font_resolves_by_family() {
        let assets_directory = tempfile::tempdir().unwrap();
        let fonts_directory = assets_directory.path().join("fonts/nested");
        std::fs::create_dir_all(&fonts_directory).unwrap();
        let installed = fonts_directory.join("Tuffy.ttf");
        let fixtures = fixture_assets();
        std::fs::write(&installed, fixtures.read("Tuffy.ttf", "file").unwrap().1).unwrap();
        let assets = Assets::new(assets_directory.path().into());
        let font = ResolvedFont {
            family: Some("Tuffy".to_owned()),
            file: None,
            ..fixture_font()
        };
        let request = TextRequest {
            text: "text",
            font: &font,
            wrap_width: None,
            align: TextAlign::Left,
            path: "font",
        };

        let layout = FontLibrary::default().layout(&request, &assets).unwrap();

        assert!(layout.font_source.ends_with("fonts/nested/Tuffy.ttf"));
    }

    #[test]
    fn a_corrupt_project_font_is_an_error_rather_than_a_missing_family() {
        let assets_directory = tempfile::tempdir().unwrap();
        let fonts_directory = assets_directory.path().join("fonts");
        std::fs::create_dir_all(&fonts_directory).unwrap();
        std::fs::write(fonts_directory.join("broken.ttf"), "not a font").unwrap();
        let assets = Assets::new(assets_directory.path().into());

        let error = FontLibrary::default().project_fonts(&assets).unwrap_err();

        assert_eq!(error.code, "invalid_font");
    }

    /// Assets holding Inter split across three subset files, as a Fontsource install leaves it.
    fn split_inter_assets() -> (tempfile::TempDir, Assets) {
        let assets_directory = tempfile::tempdir().unwrap();
        let fonts_directory = assets_directory.path().join("fonts/inter");
        std::fs::create_dir_all(&fonts_directory).unwrap();
        for subset in ["cyrillic", "latin", "vietnamese"] {
            let name = format!("Inter-{subset}-400-normal.ttf");
            let bytes = fixture_assets().read(&name, "file").unwrap().1;
            std::fs::write(fonts_directory.join(name), bytes).unwrap();
        }
        let assets = Assets::new(assets_directory.path().into());
        (assets_directory, assets)
    }

    fn inter_layout(text: &str, assets: &Assets) -> Result<TextLayout, Diagnostic> {
        let font = ResolvedFont {
            family: Some("Inter".to_owned()),
            file: None,
            ..fixture_font()
        };
        let request = TextRequest {
            text,
            font: &font,
            wrap_width: None,
            align: TextAlign::Left,
            path: "font",
        };
        FontLibrary::default().layout(&request, assets)
    }

    #[test]
    fn one_text_node_draws_from_every_subset_file_of_its_family() {
        let (_directory, assets) = split_inter_assets();

        let latin = inter_layout("Hello", &assets).unwrap();
        let cyrillic = inter_layout("Привет", &assets).unwrap();
        let mixed = inter_layout("HelloПривет", &assets).unwrap();

        assert!((mixed.width - (latin.width + cyrillic.width)).abs() < 1.0);
        let ink = mixed.outlines.unwrap().bounds();
        assert!(f64::from(ink.right()) > latin.width + cyrillic.width / 2.0);
    }

    #[test]
    fn combining_marks_compose_in_the_file_that_has_the_result() {
        let (_directory, assets) = split_inter_assets();

        // "e" with two marks: only the Vietnamese file has the composed letter.
        let decomposed = inter_layout("e\u{0323}\u{0302}", &assets).unwrap();
        let composed = inter_layout("\u{1EC7}", &assets).unwrap();

        assert_eq!(decomposed.width, composed.width);
    }

    #[test]
    fn characters_in_no_subset_file_are_still_an_error() {
        let (_directory, assets) = split_inter_assets();

        let error = inter_layout("Hello 日本語", &assets).unwrap_err();

        assert_eq!(error.code, "missing_glyphs");
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
