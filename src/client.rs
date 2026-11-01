//! Thin HTTP client for the CLI subcommands. Owner: server worker. Uses ureq.

use crate::events::CloudEvent;

pub fn base_url() -> String {
    std::env::var("TRIAGE_URL").unwrap_or_else(|_| "http://127.0.0.1:7340".to_string())
}

pub fn ingest(ev: &CloudEvent) -> Result<serde_json::Value, String> {
    let url = format!("{}/v1/ingest", base_url().trim_end_matches('/'));
    let mut response = ureq::post(&url).send_json(ev).map_err(|error| error.to_string())?;
    response.body_mut().read_json().map_err(|error| error.to_string())
}

/// GET /v1/queue.
pub fn queue() -> Result<serde_json::Value, String> {
    let url = format!("{}/v1/queue", base_url().trim_end_matches('/'));
    let mut response = ureq::get(&url).call().map_err(|error| error.to_string())?;
    response.body_mut().read_json().map_err(|error| error.to_string())
}
