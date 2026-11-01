//! Discovering fonts for agents. The same databases serve rendering, so what these commands
//! report is what a text layer will find.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use clap::ValueEnum;
use fontdb::{Database, FaceInfo, Source, Style};
use rustybuzz::Face;
use rustybuzz::ttf_parser::{self, Permissions, name_id};
use serde::{Deserialize, Serialize};

use crate::assets::{Assets, write_atomically, write_new_file};
use crate::diagnostics::Diagnostic;
use crate::fontsource::{self, CatalogFont, FontDetails, VariantSelection};
use crate::text::FontLibrary;

/// Relative to the assets directory.
const FONT_DIRECTORY: &str = "fonts";
const RECORD_FILE: &str = "font.json";
const LICENSE_FILE: &str = "LICENSE";

/// Where `fonts add` takes a font from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum InstallOrigin {
    Fontsource,
    System,
}

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

/// Where an installed font came from, kept beside its files.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledFont {
    pub family: String,
    pub source: String,
    pub license: Option<String>,
    pub license_file: Option<String>,
    pub version: Option<String>,
    pub upstream: Option<String>,
    pub files: BTreeSet<String>,
}

fn font_directory(assets_directory: &Path, id: &str) -> PathBuf {
    assets_directory.join(FONT_DIRECTORY).join(id)
}

fn file_error(path: &Path, error: std::io::Error) -> Diagnostic {
    Diagnostic::new("asset_io_error", format!("{}: {error}", path.display()))
}

fn read_record(directory: &Path) -> Result<Option<InstalledFont>, Diagnostic> {
    let path = directory.join(RECORD_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(file_error(&path, error)),
    };
    serde_json::from_str(&text).map(Some).map_err(|error| {
        Diagnostic::new("invalid_manifest", format!("{}: {error}", path.display()))
    })
}

fn already_installed(family: &str) -> Diagnostic {
    Diagnostic::new(
        "font_already_installed",
        format!("\"{family}\" is already installed; the existing files were kept"),
    )
}

fn validate_font(bytes: &[u8], name: &str) -> Result<(), Diagnostic> {
    match Face::from_slice(bytes, 0) {
        Some(_) => Ok(()),
        None => Err(Diagnostic::new(
            "invalid_font",
            format!("{name} is not a usable font"),
        )),
    }
}

/// Writes validated files and merges them into the record, so adding a weight later keeps the
/// earlier ones. Existing files are left alone.
fn install_files(
    directory: &Path,
    mut record: InstalledFont,
    files: &[(String, Vec<u8>)],
) -> Result<(), Diagnostic> {
    let existing = read_record(directory)?;
    for (name, bytes) in files {
        let path = directory.join(name);
        write_new_file(&path, bytes).map_err(|error| file_error(&path, error))?;
    }
    record
        .files
        .extend(files.iter().map(|(name, _)| name.clone()));
    if let Some(existing) = existing {
        record.files.extend(existing.files);
    }
    let path = directory.join(RECORD_FILE);
    let text = serde_json::to_string_pretty(&record).expect("records serialize to JSON");
    write_atomically(&path, format!("{text}\n").as_bytes())
        .map_err(|error| file_error(&path, error))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallReport {
    pub family: String,
    pub directory: PathBuf,
    pub installed: Vec<String>,
    pub skipped: Vec<String>,
    pub license: Option<String>,
    pub warnings: Vec<Diagnostic>,
}

pub fn add_from_fontsource(
    family: &str,
    selection: &VariantSelection,
    assets_directory: &Path,
) -> Result<InstallReport, Diagnostic> {
    let details = fontsource::details(family)?;
    let directory = font_directory(assets_directory, &details.font.id);
    let (skipped, missing): (Vec<_>, Vec<_>) = details
        .files_for(selection)?
        .into_iter()
        .partition(|file| directory.join(&file.file_name).exists());
    if missing.is_empty() {
        return Err(already_installed(&details.font.family));
    }
    let mut downloads = Vec::new();
    for file in &missing {
        let bytes = crate::net::fetch(&file.url, &[])?.ok_or_else(|| {
            Diagnostic::new("network_error", format!("{} does not exist", file.url))
        })?;
        validate_font(&bytes, &file.file_name)?;
        downloads.push((file.file_name.clone(), bytes));
    }
    let license_text = fontsource::license_text(&details.font.id)?;
    let mut warnings = Vec::new();
    if details.font.variable {
        warnings.push(Diagnostic::new(
            "font_installed_static",
            "installed static weights; Fontsource's variable files are WOFF2, which Forma cannot read",
        ));
    }
    if license_text.is_none() {
        warnings.push(Diagnostic::new(
            "font_license_missing",
            format!(
                "no license text was published; recorded {}",
                details.font.license
            ),
        ));
    }
    let license_file = license_text.as_ref().map(|_| LICENSE_FILE.to_owned());
    if let Some(text) = &license_text {
        let path = directory.join(LICENSE_FILE);
        write_new_file(&path, text).map_err(|error| file_error(&path, error))?;
    }
    let installed = downloads.iter().map(|(name, _)| name.clone()).collect();
    let record = record_for(&details, license_file);
    install_files(&directory, record, &downloads)?;
    Ok(InstallReport {
        family: details.font.family,
        directory,
        installed,
        skipped: skipped.into_iter().map(|file| file.file_name).collect(),
        license: Some(details.font.license),
        warnings,
    })
}

/// The font's license statement, unless its embedding bits forbid copying it at all.
fn license_if_copyable(
    bytes: &[u8],
    face_index: u32,
    name: &str,
) -> Result<Option<String>, Diagnostic> {
    let face = ttf_parser::Face::parse(bytes, face_index)
        .map_err(|_| Diagnostic::new("invalid_font", format!("{name} is not a usable font")))?;
    if face.permissions() == Some(Permissions::Restricted) {
        return Err(Diagnostic::new(
            "font_copy_restricted",
            format!("{name} forbids embedding, so it cannot be copied into a project"),
        ));
    }
    let statement = |id: u16| {
        face.names()
            .into_iter()
            .find(|name| name.name_id == id && name.is_unicode())
            .and_then(|name| name.to_string())
    };
    Ok(statement(name_id::LICENSE).or_else(|| statement(name_id::COPYRIGHT_NOTICE)))
}

/// Text layers already use system fonts in place, so this is only for sharing a project, and
/// a system font's license usually decides whether that is allowed. Permission bits cannot
/// prove it is, hence the explicit acceptance on top of them.
pub fn add_from_system(
    library: &FontLibrary,
    family: &str,
    license_accepted: bool,
    assets_directory: &Path,
) -> Result<InstallReport, Diagnostic> {
    let database = library.system_fonts();
    let faces: Vec<&FaceInfo> = database
        .faces()
        .filter(|face| {
            face.families
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(family))
        })
        .collect();
    if faces.is_empty() {
        return Err(Diagnostic::new(
            "font_not_found",
            format!("no system font is named \"{family}\""),
        ));
    }
    if !license_accepted {
        return Err(Diagnostic::new(
            "font_license_not_accepted",
            format!(
                "\"{family}\" already works without copying. To copy it, confirm its license \
                 allows that and pass --accept-license"
            ),
        ));
    }
    let mut files: BTreeMap<PathBuf, Vec<u8>> = BTreeMap::new();
    let mut license = None;
    for face in faces {
        let (Source::File(path) | Source::SharedFile(path, _)) = &face.source else {
            continue;
        };
        if !files.contains_key(path) {
            let bytes = fs::read(path).map_err(|error| file_error(path, error))?;
            files.insert(path.clone(), bytes);
        }
        let name = path.display().to_string();
        let statement = license_if_copyable(&files[path], face.index, &name)?;
        license = license.or(statement);
    }
    let id = family
        .trim()
        .to_lowercase()
        .replace(|character: char| !character.is_ascii_alphanumeric(), "-");
    let directory = font_directory(assets_directory, &id);
    let named_files = files
        .iter()
        .map(|(path, bytes)| {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            (name.into_owned(), bytes.clone())
        })
        .collect::<Vec<_>>();
    if named_files
        .iter()
        .any(|(name, _)| directory.join(name).exists())
    {
        return Err(already_installed(family));
    }
    let installed = named_files.iter().map(|(name, _)| name.clone()).collect();
    let record = InstalledFont {
        family: family.to_owned(),
        source: FontSource::System.name().to_owned(),
        license: license.clone(),
        license_file: None,
        version: None,
        upstream: files.keys().next().map(|path| path.display().to_string()),
        files: BTreeSet::new(),
    };
    install_files(&directory, record, &named_files)?;
    let warnings = license
        .iter()
        .map(|statement| {
            Diagnostic::new(
                "font_license_review",
                format!("copied under this statement: {statement}"),
            )
        })
        .collect();
    Ok(InstallReport {
        family: family.to_owned(),
        directory,
        installed,
        skipped: Vec::new(),
        license,
        warnings,
    })
}

fn record_for(details: &FontDetails, license_file: Option<String>) -> InstalledFont {
    InstalledFont {
        family: details.font.family.clone(),
        source: FontSource::Fontsource.name().to_owned(),
        license: Some(details.font.license.clone()),
        license_file,
        version: Some(details.version.clone()),
        upstream: Some(details.source.clone()),
        files: BTreeSet::new(),
    }
}

/// Everything known about a font, from whichever places still answer.
#[derive(Debug, Serialize)]
pub struct FontInfo {
    pub catalog: Option<FontDetails>,
    pub installed: Option<InstalledFont>,
    pub available: Option<FontFamily>,
    pub warnings: Vec<Diagnostic>,
}

/// Works offline for anything local; only a font found nowhere is an error.
pub fn info(
    library: &FontLibrary,
    assets: Option<&Assets>,
    assets_directory: Option<&Path>,
    family: &str,
) -> Result<FontInfo, Diagnostic> {
    let installed = match (assets_directory, fontsource::font_id(family)) {
        (Some(directory), Ok(id)) => read_record(&font_directory(directory, &id))?,
        _ => None,
    };
    let available = check(library, assets, family).ok();
    let mut warnings = Vec::new();
    let catalog = match fontsource::details(family) {
        Ok(details) => Some(details),
        Err(error) if installed.is_none() && available.is_none() => return Err(error),
        Err(error) if error.code == "font_not_found" => None,
        Err(error) => {
            warnings.push(error);
            None
        }
    };
    Ok(FontInfo {
        catalog,
        installed,
        available,
        warnings,
    })
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

    fn record(family: &str) -> InstalledFont {
        InstalledFont {
            family: family.to_owned(),
            source: "fontsource".to_owned(),
            license: Some("OFL-1.1".to_owned()),
            license_file: None,
            version: None,
            upstream: None,
            files: BTreeSet::new(),
        }
    }

    #[test]
    fn adding_files_later_keeps_earlier_ones_and_never_replaces_a_file() {
        let directory = tempfile::tempdir().unwrap();
        let font = directory.path().join("inter");

        let first = [("latin-400-normal.ttf".to_owned(), b"first".to_vec())];
        install_files(&font, record("Inter"), &first).unwrap();
        let second = [
            ("latin-400-normal.ttf".to_owned(), b"changed".to_vec()),
            ("latin-700-normal.ttf".to_owned(), b"bold".to_vec()),
        ];
        install_files(&font, record("Inter"), &second).unwrap();

        let saved = read_record(&font).unwrap().unwrap();
        assert_eq!(saved.files.len(), 2);
        assert_eq!(
            fs::read(font.join("latin-400-normal.ttf")).unwrap(),
            b"first"
        );
    }

    #[test]
    fn a_download_that_is_not_a_font_is_rejected() {
        assert_eq!(
            validate_font(b"<html>gateway error", "x.ttf")
                .unwrap_err()
                .code,
            "invalid_font"
        );
    }

    #[test]
    fn a_font_that_forbids_embedding_is_not_copied() {
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/Tuffy.ttf");
        let mut bytes = fs::read(fixture).unwrap();
        assert!(license_if_copyable(&bytes, 0, "Tuffy").is_ok());

        // fsType sits 8 bytes into the OS/2 table; 2 means restricted license embedding.
        let directory = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
        let table_record = (0..directory)
            .map(|index| 12 + index * 16)
            .find(|record| &bytes[*record..*record + 4] == b"OS/2")
            .unwrap();
        let offset_bytes = bytes[table_record + 8..table_record + 12]
            .try_into()
            .unwrap();
        let table = u32::from_be_bytes(offset_bytes) as usize;
        bytes[table + 8..table + 10].copy_from_slice(&2u16.to_be_bytes());

        let error = license_if_copyable(&bytes, 0, "Tuffy").unwrap_err();
        assert_eq!(error.code, "font_copy_restricted");
    }

    #[test]
    fn copying_a_system_font_needs_explicit_acceptance() {
        let assets = tempfile::tempdir().unwrap();
        let library = FontLibrary::default();
        let Some(family) = families(library.system_fonts(), FontSource::System).pop() else {
            return; // A machine without fonts has nothing to protect.
        };

        let error = add_from_system(&library, &family.family, false, assets.path()).unwrap_err();

        assert_eq!(error.code, "font_license_not_accepted");
        assert_eq!(fs::read_dir(assets.path()).unwrap().count(), 0);
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
