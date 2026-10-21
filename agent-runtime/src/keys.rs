//! API keys from the platform's vault (ADR-0051). The secret, not any spec, names the one host it may go to.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// The token is minted by the owner for exactly the `vault://<org>/<name>` refs agents may use.
pub struct Vault {
    pub url: String,
    pub org: String,
    pub token: String,
}

fn safe(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl Vault {
    pub fn from_env() -> Option<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Some(Vault {
            url: var("HOLON_PLATFORM_URL")?,
            org: var("HOLON_SECRETS_ORG")?,
            token: var("HOLON_FETCH_TOKEN")?,
        })
    }

    pub fn load(&self, name: &str) -> Result<ApiKey, String> {
        if !safe(name) || !safe(&self.org) {
            return Err(format!("`{name}` is not a key name"));
        }
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        // The platform refuses a nonce it has seen (ADR-0071).
        let nonce = format!(
            "{}-{}-{}",
            std::process::id(),
            now.as_nanos(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let url = format!(
            "{}/api/internal/secret?ref=vault%3A%2F%2F{}%2F{name}",
            self.url.trim_end_matches('/'),
            self.org
        );
        let r = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?
            .get(url)
            .header("x-fetch-token", &self.token)
            .header("x-fetch-ts", now.as_secs().to_string())
            .header("x-fetch-nonce", nonce)
            .send()
            .map_err(|e| format!("reaching the vault: {e}"))?;
        match r.status().as_u16() {
            200 => {}
            401 => return Err("the vault fetch token is unknown or expired".into()),
            403 => return Err(format!("the fetch token does not grant the API key {name}")),
            404 => return Err(format!("no API key called {name}")),
            code => return Err(format!("the vault answered {code}")),
        }
        let text = r.text().map_err(|e| format!("reading API key {name}: {e}"))?;
        // Not serde's message: it can quote the key.
        let mut k: ApiKey = serde_json::from_str(&text)
            .map_err(|_| format!("API key {name} must be JSON with `host` and `key`"))?;
        k.key = k.key.trim().to_string();
        if k.key.is_empty() || k.host.is_empty() {
            return Err(format!("API key {name} needs a `host` and a `key`"));
        }
        Ok(k)
    }
}
