//! Lucide icons through the Iconify API. No other collection is supported.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

use crate::assets::{is_safe_slug, validate_svg, write_atomically, write_new_file};
use crate::diagnostics::Diagnostic;
use crate::net;

const API: &str = "https://api.iconify.design";
const COLLECTION: &str = "lucide";
/// Relative to the assets directory, as frames reference it.
const ICON_DIRECTORY: &str = "icons/lucide";
const MANIFEST_FILE: &str = "manifest.json";
const LICENSE: &str = "ISC";
const LICENSE_URL: &str = "https://github.com/lucide-icons/lucide/blob/main/LICENSE";
/// The largest page the search endpoint serves.
const SEARCH_LIMIT: &str = "999";

fn not_found(name: &str) -> Diagnostic {
    Diagnostic::new(
        "icon_not_found",
        format!("Lucide has no icon named \"{name}\"; try `forma icons search`"),
    )
}

/// Names become paths and URLs, so anything but a plain slug stops here.
fn validate_name(name: &str) -> Result<(), Diagnostic> {
    if is_safe_slug(name) {
        return Ok(());
    }
    Err(Diagnostic::new(
        "invalid_icon_name",
        format!("\"{name}\" is not an icon name; names look like \"arrow-right\""),
    ))
}

fn source_url(name: &str) -> String {
    format!("{API}/{COLLECTION}/{name}.svg")
}

#[derive(Deserialize)]
struct SearchResponse {
    icons: Vec<String>,
}

/// The API prefixes names with their collection; anything from elsewhere is dropped.
fn lucide_names(response: SearchResponse) -> Vec<String> {
    let prefix = format!("{COLLECTION}:");
    response
        .icons
        .iter()
        .filter_map(|icon| icon.strip_prefix(&prefix))
        .map(str::to_owned)
        .collect()
}

pub fn search(query: &str) -> Result<Vec<String>, Diagnostic> {
    let url = format!("{API}/search");
    let parameters = [
        ("query", query),
        ("prefix", COLLECTION),
        ("limit", SEARCH_LIMIT),
    ];
    let body = net::fetch(&url, &parameters)?
        .ok_or_else(|| Diagnostic::new("network_error", format!("{url} does not exist")))?;
    Ok(lucide_names(net::parse_json(&body, &url)?))
}

#[derive(Deserialize)]
struct IconSetResponse {
    width: u32,
    height: u32,
    #[serde(default)]
    icons: HashMap<String, IgnoredAny>,
    #[serde(default)]
    aliases: HashMap<String, IgnoredAny>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IconInfo {
    pub name: String,
    pub collection: &'static str,
    pub license: String,
    pub license_url: String,
    pub source: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// The path a frame's `src` uses; `None` until installed.
    pub src: Option<String>,
}

/// Installed icons answer from disk, so `info` keeps working offline.
pub fn info(name: &str, assets_directory: &Path) -> Result<IconInfo, Diagnostic> {
    validate_name(name)?;
    if assets_directory.join(frame_source(name)).is_file() {
        let manifest = read_manifest(assets_directory)?;
        let record = manifest.icons.get(name);
        return Ok(IconInfo {
            name: name.to_owned(),
            collection: COLLECTION,
            license: record.map_or(LICENSE, |record| &record.license).to_owned(),
            license_url: record
                .map_or(LICENSE_URL, |record| &record.license_url)
                .to_owned(),
            source: record.map_or_else(|| source_url(name), |record| record.source.clone()),
            width: None,
            height: None,
            src: Some(frame_source(name)),
        });
    }
    let url = format!("{API}/{COLLECTION}.json");
    let body = net::fetch(&url, &[("icons", name)])?.ok_or_else(|| not_found(name))?;
    let set: IconSetResponse = net::parse_json(&body, &url)?;
    if !set.icons.contains_key(name) && !set.aliases.contains_key(name) {
        return Err(not_found(name));
    }
    Ok(IconInfo {
        name: name.to_owned(),
        collection: COLLECTION,
        license: LICENSE.to_owned(),
        license_url: LICENSE_URL.to_owned(),
        source: source_url(name),
        width: Some(set.width),
        height: Some(set.height),
        src: None,
    })
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IconRecord {
    source: String,
    license: String,
    license_url: String,
}

/// Where each installed icon came from. Sorted so diffs stay small in Git.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    icons: BTreeMap<String, IconRecord>,
}

fn manifest_path(assets_directory: &Path) -> std::path::PathBuf {
    assets_directory.join(ICON_DIRECTORY).join(MANIFEST_FILE)
}

fn read_manifest(assets_directory: &Path) -> Result<Manifest, Diagnostic> {
    let path = manifest_path(assets_directory);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Manifest::default());
        }
        Err(error) => return Err(file_error(&path, error)),
    };
    serde_json::from_str(&text).map_err(|error| {
        Diagnostic::new("invalid_manifest", format!("{}: {error}", path.display()))
    })
}

fn file_error(path: &Path, error: std::io::Error) -> Diagnostic {
    Diagnostic::new("asset_io_error", format!("{}: {error}", path.display()))
}

fn frame_source(name: &str) -> String {
    format!("{ICON_DIRECTORY}/{name}.svg")
}

/// The frame `src` of the new icon. Everything is checked before anything is written.
fn install(name: &str, svg: &[u8], assets_directory: &Path) -> Result<String, Diagnostic> {
    validate_svg(svg)?;
    let mut manifest = read_manifest(assets_directory)?;
    let src = frame_source(name);
    let path = assets_directory.join(&src);
    if !write_new_file(&path, svg).map_err(|error| file_error(&path, error))? {
        return Err(already_installed(name));
    }
    manifest.icons.insert(
        name.to_owned(),
        IconRecord {
            source: source_url(name),
            license: LICENSE.to_owned(),
            license_url: LICENSE_URL.to_owned(),
        },
    );
    let path = manifest_path(assets_directory);
    let text = serde_json::to_string_pretty(&manifest).expect("manifests serialize to JSON");
    write_atomically(&path, format!("{text}\n").as_bytes())
        .map_err(|error| file_error(&path, error))?;
    Ok(src)
}

fn already_installed(name: &str) -> Diagnostic {
    Diagnostic::new(
        "icon_already_installed",
        format!("\"{name}\" is already installed; the existing file was kept"),
    )
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledIcon {
    pub name: String,
    pub src: String,
    pub license: Option<String>,
}

pub fn add(name: &str, assets_directory: &Path) -> Result<InstalledIcon, Diagnostic> {
    validate_name(name)?;
    // Failing here keeps a re-add working offline and off the network.
    if assets_directory.join(frame_source(name)).exists() {
        return Err(already_installed(name));
    }
    let svg = net::fetch(&source_url(name), &[])?.ok_or_else(|| not_found(name))?;
    Ok(InstalledIcon {
        name: name.to_owned(),
        src: install(name, &svg, assets_directory)?,
        license: Some(LICENSE.to_owned()),
    })
}

/// Every SVG in the directory, so hand-placed files appear too, without a recorded license.
pub fn list(assets_directory: &Path) -> Result<Vec<InstalledIcon>, Diagnostic> {
    let manifest = read_manifest(assets_directory)?;
    let directory = assets_directory.join(ICON_DIRECTORY);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(file_error(&directory, error)),
    };
    let mut icons = Vec::new();
    for entry in entries {
        let path = entry.map_err(|error| file_error(&directory, error))?.path();
        if path.extension().is_none_or(|extension| extension != "svg") {
            continue;
        }
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        icons.push(InstalledIcon {
            src: frame_source(&name),
            license: manifest
                .icons
                .get(&name)
                .map(|record| record.license.clone()),
            name,
        });
    }
    icons.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(icons)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_keeps_only_lucide_results() {
        let response = SearchResponse {
            icons: vec!["lucide:star".to_owned(), "mdi:star".to_owned()],
        };

        assert_eq!(lucide_names(response), ["star"]);
    }

    #[test]
    fn names_that_could_escape_a_path_or_url_are_rejected() {
        for name in ["../secret", "a/b", "a?b", "", "-flag", "Star"] {
            assert_eq!(validate_name(name).unwrap_err().code, "invalid_icon_name");
        }
        assert!(validate_name("arrow-right-2").is_ok());
    }

    const STAR: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M0 0h9v9z"/></svg>"#;

    #[test]
    fn installing_keeps_the_exact_bytes_and_records_the_license() {
        let assets = tempfile::tempdir().unwrap();

        assert_eq!(
            install("star", STAR.as_bytes(), assets.path()).unwrap(),
            "icons/lucide/star.svg"
        );

        let saved = fs::read_to_string(assets.path().join("icons/lucide/star.svg")).unwrap();
        assert_eq!(saved, STAR);
        let listed = list(assets.path()).unwrap();
        assert_eq!(listed[0].license.as_deref(), Some("ISC"));
    }

    #[test]
    fn an_existing_asset_is_never_overwritten() {
        let assets = tempfile::tempdir().unwrap();
        let path = assets.path().join("icons/lucide/star.svg");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "hand edited").unwrap();

        let error = install("star", STAR.as_bytes(), assets.path()).unwrap_err();

        assert_eq!(error.code, "icon_already_installed");
        assert_eq!(fs::read_to_string(&path).unwrap(), "hand edited");
    }

    #[test]
    fn an_invalid_svg_writes_nothing() {
        let assets = tempfile::tempdir().unwrap();

        let error = install("star", b"<html>not svg", assets.path()).unwrap_err();

        assert_eq!(error.code, "invalid_svg");
        assert!(list(assets.path()).unwrap().is_empty());
    }
}
