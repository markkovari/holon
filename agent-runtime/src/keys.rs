//! An API key the owner keeps on disk, bound by that file (not by any spec) to the one host it may go to.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct ApiKey {
    pub host: String,
    #[serde(default = "default_header")]
    pub header: String,
    /// Empty sends the bare key, e.g. for an `X-Api-Key` header.
    #[serde(default = "default_scheme")]
    pub scheme: String,
    pub key: String,
}

fn default_header() -> String {
    "Authorization".into()
}
fn default_scheme() -> String {
    "Bearer".into()
}

impl ApiKey {
    pub fn header_value(&self) -> String {
        if self.scheme.is_empty() {
            self.key.clone()
        } else {
            format!("{} {}", self.scheme, self.key)
        }
    }
}

pub fn dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".comp-secrets/keys")
}

pub fn load(dir: &Path, name: &str) -> Result<ApiKey, String> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(format!("`{name}` is not a key name"));
    }
    let text = std::fs::read_to_string(dir.join(format!("{name}.json")))
        .map_err(|_| format!("no API key called {name}"))?;
    // Not serde's message: it can quote the key.
    let mut k: ApiKey = serde_json::from_str(&text)
        .map_err(|_| format!("API key {name} must be JSON with `host` and `key`"))?;
    k.key = k.key.trim().to_string();
    if k.key.is_empty() || k.host.is_empty() {
        return Err(format!("API key {name} needs a `host` and a `key`"));
    }
    Ok(k)
}
