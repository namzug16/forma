//! HTTPS downloads for the install commands. Rendering never reaches this module.

use std::time::Duration;

use serde::de::DeserializeOwned;
use ureq::Agent;

use crate::diagnostics::Diagnostic;

const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;
const USER_AGENT: &str = concat!("forma/", env!("CARGO_PKG_VERSION"));

fn network_error(url: &str, problem: impl std::fmt::Display) -> Diagnostic {
    Diagnostic::new("network_error", format!("cannot download {url}: {problem}"))
}

/// The response body, or `None` when the server reports 404.
pub fn fetch(url: &str, query: &[(&str, &str)]) -> Result<Option<Vec<u8>>, Diagnostic> {
    let agent: Agent = Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .user_agent(USER_AGENT)
        .build()
        .into();
    let mut response = agent
        .get(url)
        .query_pairs(query.iter().copied())
        .call()
        .map_err(|error| network_error(url, error))?;
    let status = response.status();
    if status == 404 {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(network_error(url, format!("server returned {status}")));
    }
    response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BYTES)
        .read_to_vec()
        .map(Some)
        .map_err(|error| network_error(url, error))
}

pub fn parse_json<T: DeserializeOwned>(bytes: &[u8], url: &str) -> Result<T, Diagnostic> {
    serde_json::from_slice(bytes)
        .map_err(|error| network_error(url, format!("unexpected response: {error}")))
}
