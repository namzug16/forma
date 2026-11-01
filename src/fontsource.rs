//! The Fontsource catalog. Only commands use this; rendering never downloads a font.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::assets::is_safe_slug;
use crate::diagnostics::Diagnostic;
use crate::net;

const API: &str = "https://api.fontsource.org/v1/fonts";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogFont {
    pub id: String,
    pub family: String,
    pub subsets: Vec<String>,
    pub weights: Vec<u16>,
    pub styles: Vec<String>,
    #[serde(rename(deserialize = "defSubset"))]
    pub default_subset: String,
    pub variable: bool,
    pub license: String,
    pub category: String,
}

fn not_found(family: &str) -> Diagnostic {
    Diagnostic::new(
        "font_not_found",
        format!("Fontsource has no font named \"{family}\"; try `forma fonts search`"),
    )
}

/// Fontsource ids are the lowercase family name with hyphens for spaces.
pub fn font_id(family: &str) -> Result<String, Diagnostic> {
    let id = family.trim().to_lowercase().replace(' ', "-");
    if is_safe_slug(&id) {
        return Ok(id);
    }
    Err(not_found(family))
}

fn matching(catalog: Vec<CatalogFont>, query: &str) -> Vec<CatalogFont> {
    let query = query.to_lowercase();
    catalog
        .into_iter()
        .filter(|font| font.family.to_lowercase().contains(&query))
        .collect()
}

/// The API only matches whole families, so substring search filters the full catalog.
pub fn search(query: &str) -> Result<Vec<CatalogFont>, Diagnostic> {
    let body = net::fetch(API, &[])?
        .ok_or_else(|| Diagnostic::new("network_error", format!("{API} does not exist")))?;
    Ok(matching(net::parse_json(&body, API)?, query))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FontDetails {
    #[serde(flatten)]
    pub font: CatalogFont,
    pub version: String,
    /// The upstream project.
    pub source: String,
    /// Weight, then style, then subset.
    #[serde(default, skip_serializing)]
    variants: BTreeMap<String, BTreeMap<String, BTreeMap<String, VariantFile>>>,
}

#[derive(Debug, Deserialize)]
struct VariantFile {
    url: VariantUrls,
}

#[derive(Debug, Deserialize)]
struct VariantUrls {
    ttf: Option<String>,
}

/// Empty lists mean everything the font offers.
#[derive(Debug, Default)]
pub struct VariantSelection {
    pub weights: Vec<u16>,
    pub styles: Vec<String>,
    pub subsets: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub struct DownloadableFile {
    pub file_name: String,
    pub url: String,
}

fn check_offered<T: PartialEq + std::fmt::Display>(
    requested: &[T],
    offered: &[T],
    kind: &str,
    family: &str,
) -> Result<(), Diagnostic> {
    match requested.iter().find(|item| !offered.contains(item)) {
        None => Ok(()),
        Some(item) => Err(Diagnostic::new(
            "font_variant_unavailable",
            format!("{family} has no {kind} {item}"),
        )),
    }
}

impl FontDetails {
    /// Static TTFs for the selection. Fontsource's variable fonts are WOFF2 only, which the
    /// renderer cannot read. Numbered CJK slices are not in `subsets`, whose named files cover
    /// the same characters, so they are left out.
    pub fn files_for(
        &self,
        selection: &VariantSelection,
    ) -> Result<Vec<DownloadableFile>, Diagnostic> {
        let font = &self.font;
        check_offered(&selection.weights, &font.weights, "weight", &font.family)?;
        check_offered(&selection.styles, &font.styles, "style", &font.family)?;
        check_offered(&selection.subsets, &font.subsets, "subset", &font.family)?;
        let chosen =
            |requested: &[String], item: &String| requested.is_empty() || requested.contains(item);
        let weights: Vec<String> = selection.weights.iter().map(u16::to_string).collect();
        let mut files = Vec::new();
        for (weight, styles) in &self.variants {
            for (style, subsets) in styles {
                for (subset, file) in subsets {
                    let is_selected = chosen(&weights, weight)
                        && chosen(&selection.styles, style)
                        && chosen(&selection.subsets, subset)
                        && font.subsets.contains(subset);
                    if let (true, Some(url)) = (is_selected, &file.url.ttf) {
                        files.push(DownloadableFile {
                            file_name: safe_file_name(subset, weight, style)?,
                            url: url.clone(),
                        });
                    }
                }
            }
        }
        if files.is_empty() {
            return Err(Diagnostic::new(
                "font_variant_unavailable",
                format!("{} has no TTF for this selection", font.family),
            ));
        }
        Ok(files)
    }
}

/// These parts come from the API and become a path, so they are checked like user input.
fn safe_file_name(subset: &str, weight: &str, style: &str) -> Result<String, Diagnostic> {
    if [subset, weight, style].into_iter().all(is_safe_slug) {
        return Ok(format!("{subset}-{weight}-{style}.ttf"));
    }
    Err(Diagnostic::new(
        "network_error",
        format!("Fontsource returned an unsafe variant name: {subset}-{weight}-{style}"),
    ))
}

/// The font's license text, when its package publishes one.
pub fn license_text(id: &str) -> Result<Option<Vec<u8>>, Diagnostic> {
    net::fetch(
        &format!("https://cdn.jsdelivr.net/npm/@fontsource/{id}/LICENSE"),
        &[],
    )
}

pub fn details(family: &str) -> Result<FontDetails, Diagnostic> {
    let id = font_id(family)?;
    let url = format!("{API}/{id}");
    let body = net::fetch(&url, &[])?.ok_or_else(|| not_found(family))?;
    net::parse_json(&body, &url)
}
