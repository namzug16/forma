//! Lucide icons through the Iconify API. No other collection is supported.

use std::collections::HashMap;

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

use crate::assets::is_safe_slug;
use crate::diagnostics::Diagnostic;
use crate::net;

const API: &str = "https://api.iconify.design";
const COLLECTION: &str = "lucide";
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
}

pub fn info(name: &str) -> Result<IconInfo, Diagnostic> {
    validate_name(name)?;
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
    })
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
}
