//! One HTTP client for the key server, Dokploy and the sops download.

use std::time::Duration;

/// HTTP statuses come back as responses, not errors, so callers can read error bodies.
pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(60)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .user_agent(concat!("tierkey/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// The `error` field of a JSON error body, or the body itself.
pub fn error_text(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or_else(|| body.trim().chars().take(200).collect())
}
