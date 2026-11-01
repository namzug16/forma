//! The Fontsource catalog. Only commands use this; rendering never downloads a font.

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
}

pub fn details(family: &str) -> Result<FontDetails, Diagnostic> {
    let id = font_id(family)?;
    let url = format!("{API}/{id}");
    let body = net::fetch(&url, &[])?.ok_or_else(|| not_found(family))?;
    net::parse_json(&body, &url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_names_become_ids_and_unsafe_ones_are_rejected() {
        assert_eq!(font_id("Inter Tight").unwrap(), "inter-tight");
        assert_eq!(font_id("../secret").unwrap_err().code, "font_not_found");
    }

    #[test]
    fn a_real_catalog_entry_parses_and_matches_by_substring() {
        let entry = r#"{ "id": "inter-tight", "family": "Inter Tight", "subsets": ["latin"],
            "weights": [400], "styles": ["normal"], "defSubset": "latin", "variable": true,
            "lastModified": "2025-09-10", "category": "sans-serif", "license": "OFL-1.1",
            "type": "google" }"#;
        let font: CatalogFont = serde_json::from_str(entry).unwrap();

        assert_eq!(font.default_subset, "latin");
        assert_eq!(matching(vec![font], "inter").len(), 1);
    }
}
