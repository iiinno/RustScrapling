use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchTier {
    Auto,
    Http,
    Browser,
    Headed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchRequest {
    pub url: String,
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: Option<Vec<u8>>,
    #[serde(default = "default_tier")]
    pub tier: FetchTier,
}

fn default_method() -> String {
    "GET".into()
}
fn default_tier() -> FetchTier {
    FetchTier::Auto
}

impl FetchRequest {
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            method: default_method(),
            headers: BTreeMap::new(),
            body: None,
            tier: FetchTier::Auto,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchResult {
    pub status: u16,
    pub final_url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub tier: FetchTier,
    pub protocol: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub pid: u32,
    pub address: String,
    pub requests: u64,
    pub upstream_requests: u64,
    pub coalesced_requests: u64,
    pub cache_entries: usize,
    pub learned_sites: usize,
}

/// Sanitized metadata for one completed upstream daemon fetch.
/// Request and response headers, body, credentials and query strings are not recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkRecord {
    pub url: String,
    pub method: String,
    pub status: u16,
    pub tier: FetchTier,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum DaemonRequest {
    Status,
    Stop,
    Fetch { request: FetchRequest },
    NetworkSearch { url_contains: String, limit: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum DaemonResponse {
    Status(Status),
    Stopping,
    Fetch(FetchResult),
    NetworkSearch(Vec<NetworkRecord>),
    Error(DaemonError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonError {
    pub kind: String,
    pub message: String,
    pub hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

impl DaemonError {
    pub fn new(kind: &str, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            hint: String::new(),
            status: None,
        }
    }
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}
impl std::error::Error for DaemonError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_status_preserves_legacy_error_wire() {
        let legacy = r#"{"kind":"empty","message":"empty body","hint":"inspect"}"#;
        let mut error: DaemonError = serde_json::from_str(legacy).unwrap();
        assert_eq!(error.status, None);
        assert!(serde_json::to_value(&error)
            .unwrap()
            .get("status")
            .is_none());
        error.status = Some(204);
        let encoded = serde_json::to_vec(&error).unwrap();
        let decoded: DaemonError = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.status, Some(204));
        assert_eq!(decoded.kind, "empty");
    }
}
