//! API keys from the platform's vault (ADR-0051). The secret, not any spec, names the one host it may go to.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

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
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Vault {
    pub url: String,
    pub org: String,
    pub token: String,
}

pub fn safe(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl Vault {
    /// The environment, else what the console last wrote to `file()`; read at each call, so a new token needs no restart.
    pub fn configured() -> Option<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let env = || {
            Some(Vault {
                url: var("HOLON_PLATFORM_URL")?,
                org: var("HOLON_SECRETS_ORG")?,
                token: var("HOLON_FETCH_TOKEN")?,
            })
        };
        env().or_else(|| serde_json::from_str(&std::fs::read_to_string(Self::file()).ok()?).ok())
    }

    pub fn file() -> PathBuf {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".comp-secrets/vault.json")
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        // Created private, not chmod-ed after: the token is readable by no one else for any instant.
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        std::io::Write::write_all(&mut opts.open(path).map_err(|e| e.to_string())?, text.as_bytes())
            .map_err(|e| e.to_string())
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

/// What the vault holds for a key: a pasted `https://api.x.com/v1` names the host `api.x.com`,
/// and any header but `Authorization` gets the bare key.
pub fn secret_value(host: &str, key: &str, header: &str) -> Result<String, String> {
    let host = host.trim();
    let host =
        host.strip_prefix("https://").or_else(|| host.strip_prefix("http://")).unwrap_or(host);
    let host = host.split(['/', '?', '#']).next().unwrap_or_default().to_lowercase();
    let (key, header) = (key.trim(), header.trim());
    if host.is_empty() || host.contains('@') || key.is_empty() {
        return Err("a key needs a host and the key itself".into());
    }
    let mut v = serde_json::json!({ "host": host, "key": key });
    if !header.is_empty() && !header.eq_ignore_ascii_case("authorization") {
        v["header"] = header.into();
        v["scheme"] = "".into();
    }
    Ok(v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_url_names_its_host_and_a_custom_header_sends_the_bare_key() {
        let v: serde_json::Value = serde_json::from_str(
            &secret_value(" https://API.github.com/repos?x=1 ", " ghp_1 ", "").unwrap(),
        )
        .unwrap();
        assert_eq!(v, serde_json::json!({"host": "api.github.com", "key": "ghp_1"}));
        let v: serde_json::Value =
            serde_json::from_str(&secret_value("api.x.com:8443", "k", "X-Api-Key").unwrap())
                .unwrap();
        assert_eq!(v["host"], "api.x.com:8443");
        assert_eq!((v["header"].as_str(), v["scheme"].as_str()), (Some("X-Api-Key"), Some("")));
        let k: ApiKey = serde_json::from_value(v).unwrap();
        assert_eq!(k.header_value(), "k");
        assert!(secret_value("", "k", "").is_err() && secret_value("h", " ", "").is_err());
        assert!(secret_value("user@evil.test", "k", "").is_err());
    }

    #[test]
    fn the_console_writes_the_vault_file_only_its_owner_can_read() {
        let p = crate::testutil::dir("vault").join("sub/vault.json");
        let v = Vault { url: "http://127.0.0.1:1".into(), org: "me".into(), token: "t".into() };
        v.save(&p).unwrap();
        let back: Vault = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(back, v);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}
