//! SurrealDB databases, through the server's own `/export` and `/import`.
//!
//! An export is SurrealQL that recreates the database — definitions and
//! records — so it is the server's format, readable by the same server
//! version's import, and nothing here parses it.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use tokio::io::AsyncWriteExt;

#[derive(clap::Args, Debug, Clone, Default)]
pub struct SurrealArgs {
    /// SurrealDB HTTP endpoint, e.g. `http://127.0.0.1:8000`.
    #[arg(long)]
    pub surreal_url: Option<String>,
    /// `namespace/database`, repeatable.
    #[arg(long = "surreal-db")]
    pub surreal_dbs: Vec<String>,
    #[arg(long, default_value = "root")]
    pub surreal_user: String,
    /// File holding the password; unauthenticated when absent.
    #[arg(long)]
    pub surreal_pass_file: Option<String>,
}

pub struct Surreal {
    http: reqwest::Client,
    url: String,
    auth: Option<String>,
}

pub fn split(nsdb: &str) -> Result<(&str, &str)> {
    nsdb.split_once('/')
        .filter(|(n, d)| !n.is_empty() && !d.is_empty() && !d.contains('/'))
        .with_context(|| format!("--surreal-db {nsdb}: expected namespace/database"))
}

impl Surreal {
    pub fn new(a: &SurrealArgs) -> Result<Option<Self>> {
        let Some(url) = &a.surreal_url else {
            return Ok(None);
        };
        let auth = match &a.surreal_pass_file {
            Some(p) => {
                let pass = std::fs::read_to_string(p).with_context(|| format!("reading {p}"))?;
                let pair = format!("{}:{}", a.surreal_user, pass.trim());
                Some(format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(pair)))
            }
            None => None,
        };
        Ok(Some(Self {
            http: reqwest::Client::builder().timeout(Duration::from_secs(3600)).build()?,
            url: url.trim_end_matches('/').to_string(),
            auth,
        }))
    }

    fn req(
        &self,
        method: reqwest::Method,
        path: &str,
        ns: &str,
        db: &str,
    ) -> reqwest::RequestBuilder {
        let mut r = self
            .http
            .request(method, format!("{}{path}", self.url))
            .header("surreal-ns", ns)
            .header("surreal-db", db);
        if let Some(a) = &self.auth {
            r = r.header("authorization", a);
        }
        r
    }

    pub async fn export(&self, nsdb: &str, path: &Path) -> Result<u64> {
        let (ns, db) = split(nsdb)?;
        let mut resp = self
            .req(reqwest::Method::GET, "/export", ns, db)
            .header("accept", "application/octet-stream")
            .send()
            .await
            .with_context(|| format!("exporting {nsdb}"))?;
        if !resp.status().is_success() {
            let s = resp.status();
            bail!("exporting {nsdb}: HTTP {s} {}", resp.text().await.unwrap_or_default());
        }
        let mut file = tokio::fs::File::create(path).await?;
        let mut n = 0u64;
        while let Some(chunk) = resp.chunk().await? {
            n += chunk.len() as u64;
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        Ok(n)
    }

    pub async fn import(&self, nsdb: &str, surql: Vec<u8>) -> Result<()> {
        let (ns, db) = split(nsdb)?;
        let resp = self
            .req(reqwest::Method::POST, "/import", ns, db)
            .header("accept", "application/json")
            .body(surql)
            .send()
            .await
            .with_context(|| format!("importing {nsdb}"))?;
        let (s, body) = (resp.status(), resp.text().await.unwrap_or_default());
        if !s.is_success() {
            bail!("importing {nsdb}: HTTP {s} {body}");
        }
        // A 200 whose statements failed is SurrealDB's normal shape for that.
        if let Ok(serde_json::Value::Array(stmts)) = serde_json::from_str(&body) {
            if let Some(bad) = stmts.iter().find(|s| s["status"] != "OK") {
                bail!("importing {nsdb}: {}", bad["result"]);
            }
        }
        Ok(())
    }
}
