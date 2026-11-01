//! Discovering fonts for agents. The same databases serve rendering, so what these commands
//! report is what a text layer will find.

use std::collections::{BTreeMap, BTreeSet};

use clap::ValueEnum;
use fontdb::{Database, FaceInfo, Style};
use serde::Serialize;

use crate::assets::Assets;
use crate::diagnostics::Diagnostic;
use crate::fontsource::{self, CatalogFont};
use crate::text::FontLibrary;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum FontSource {
    Project,
    System,
    #[value(skip)]
    Fontsource,
}

impl FontSource {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::System => "system",
            Self::Fontsource => "fontsource",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct FontFamily {
    pub family: String,
    pub source: FontSource,
    pub weights: BTreeSet<u16>,
    pub styles: BTreeSet<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
}

fn style_name(style: Style) -> &'static str {
    match style {
        Style::Normal => "normal",
        Style::Italic => "italic",
        Style::Oblique => "oblique",
    }
}

fn add_face(family: &mut FontFamily, face: &FaceInfo) {
    family.weights.insert(face.weight.0);
    family.styles.insert(style_name(face.style).to_owned());
}

fn new_family(name: &str, source: FontSource) -> FontFamily {
    FontFamily {
        family: name.to_owned(),
        source,
        weights: BTreeSet::new(),
        styles: BTreeSet::new(),
        license: None,
    }
}

/// Faces grouped under their primary family name, sorted by it.
fn families(database: &Database, source: FontSource) -> Vec<FontFamily> {
    let mut families: BTreeMap<&str, FontFamily> = BTreeMap::new();
    for face in database.faces() {
        // A leading dot marks macOS's private interface fonts.
        let Some((name, _)) = face
            .families
            .first()
            .filter(|(name, _)| !name.starts_with('.'))
        else {
            continue;
        };
        let family = families
            .entry(name)
            .or_insert_with(|| new_family(name, source));
        add_face(family, face);
    }
    families.into_values().collect()
}

/// Matches any of a face's family names, as text layers do.
fn find_family(database: &Database, name: &str, source: FontSource) -> Option<FontFamily> {
    let mut found = None;
    for face in database.faces().filter(|face| {
        face.families
            .iter()
            .any(|(family, _)| family.eq_ignore_ascii_case(name))
    }) {
        add_face(found.get_or_insert_with(|| new_family(name, source)), face);
    }
    found
}

/// Project fonts first, the order text layers resolve them in. `assets` is `None` outside a
/// project, where only system fonts exist.
pub fn list(
    library: &FontLibrary,
    assets: Option<&Assets>,
    source: Option<FontSource>,
) -> Result<Vec<FontFamily>, Diagnostic> {
    let mut all_families = Vec::new();
    if let (Some(assets), None | Some(FontSource::Project)) = (assets, source) {
        let project = library.project_fonts(assets)?;
        all_families.extend(families(&project.database, FontSource::Project));
    }
    if matches!(source, None | Some(FontSource::System)) {
        all_families.extend(families(library.system_fonts(), FontSource::System));
    }
    Ok(all_families)
}

impl From<CatalogFont> for FontFamily {
    fn from(font: CatalogFont) -> Self {
        Self {
            family: font.family,
            source: FontSource::Fontsource,
            weights: font.weights.into_iter().collect(),
            styles: font.styles.into_iter().collect(),
            license: Some(font.license),
        }
    }
}

/// What `search` found, and why part of it may be missing.
#[derive(Debug, Serialize)]
pub struct SearchResults {
    pub fonts: Vec<FontFamily>,
    pub warnings: Vec<Diagnostic>,
}

/// Local fonts first. Losing the network only costs the catalog, unless nothing else matched.
pub fn search(
    library: &FontLibrary,
    assets: Option<&Assets>,
    query: &str,
) -> Result<SearchResults, Diagnostic> {
    let lowercase_query = query.to_lowercase();
    let mut fonts: Vec<FontFamily> = list(library, assets, None)?
        .into_iter()
        .filter(|font| font.family.to_lowercase().contains(&lowercase_query))
        .collect();
    let mut warnings = Vec::new();
    match fontsource::search(query) {
        Ok(catalog) => fonts.extend(catalog.into_iter().map(FontFamily::from)),
        Err(error) if fonts.is_empty() => return Err(error),
        Err(error) => warnings.push(error),
    }
    Ok(SearchResults { fonts, warnings })
}

/// The family a text layer naming `name` would use.
pub fn check(
    library: &FontLibrary,
    assets: Option<&Assets>,
    name: &str,
) -> Result<FontFamily, Diagnostic> {
    let in_project = match assets {
        Some(assets) => find_family(
            &library.project_fonts(assets)?.database,
            name,
            FontSource::Project,
        ),
        None => None,
    };
    in_project
        .or_else(|| find_family(library.system_fonts(), name, FontSource::System))
        .ok_or_else(|| {
            Diagnostic::new(
                "font_not_found",
                format!("font \"{name}\" is not available"),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn assets_with_tuffy() -> (tempfile::TempDir, Assets) {
        let directory = tempfile::tempdir().unwrap();
        let fonts = directory.path().join("fonts");
        fs::create_dir(&fonts).unwrap();
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/Tuffy.ttf");
        fs::copy(fixture, fonts.join("Tuffy.ttf")).unwrap();
        let assets = Assets::new(directory.path().into());
        (directory, assets)
    }

    #[test]
    fn check_reports_project_fonts_case_insensitively_and_misses_as_errors() {
        let (_directory, assets) = assets_with_tuffy();
        let library = FontLibrary::default();

        let found = check(&library, Some(&assets), "tuffy").unwrap();
        assert_eq!(found.source, FontSource::Project);
        assert!(found.styles.contains("normal"));

        let missing = check(&library, Some(&assets), "No Such Family Anywhere").unwrap_err();
        assert_eq!(missing.code, "font_not_found");
    }

    #[test]
    fn listing_can_be_limited_to_project_fonts() {
        let (_directory, assets) = assets_with_tuffy();
        let library = FontLibrary::default();

        let families = list(&library, Some(&assets), Some(FontSource::Project)).unwrap();

        assert_eq!(families.len(), 1);
        assert_eq!(families[0].family, "Tuffy");
    }
}
