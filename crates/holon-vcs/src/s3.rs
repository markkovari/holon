//! S3-compatible blob storage adapter (ADR-0122).
//!
//! Stores immutable, content-addressed bytes in an S3 bucket keyed by SHA-256.

use std::time::Duration;

use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};

use crate::error::{Result, VcsError};
use crate::model::Hash;
use crate::store::{sha256_hex, BlobStore};

const SIGN_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone)]
pub struct S3Config {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    pub path_style: bool,
    pub prefix: String,
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            endpoint: "http://127.0.0.1:9000".into(),
            bucket: "holon-vcs-blobs".into(),
            region: "us-east-1".into(),
            access_key: "rustfsadmin".into(),
            secret_key: "rustfsadmin".into(),
            path_style: true,
            prefix: String::new(),
        }
    }
}

pub struct S3Blobs {
    bucket: Bucket,
    creds: Credentials,
    http: reqwest::Client,
    prefix: String,
}

impl S3Blobs {
    pub async fn new(cfg: S3Config) -> Result<Self> {
        let endpoint = cfg
            .endpoint
            .parse()
            .map_err(|e| VcsError::storage(format!("invalid S3 endpoint {}: {e}", cfg.endpoint)))?;
        let style = if cfg.path_style { UrlStyle::Path } else { UrlStyle::VirtualHost };
        let bucket = Bucket::new(endpoint, style, cfg.bucket, cfg.region)
            .map_err(|e| VcsError::storage(format!("invalid S3 bucket: {e}")))?;
        let creds = Credentials::new(cfg.access_key, cfg.secret_key);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(VcsError::storage)?;

        let store = Self { bucket, creds, http, prefix: cfg.prefix };
        store.ensure_bucket().await?;
        Ok(store)
    }

    fn key(&self, hash: &str) -> String {
        if self.prefix.is_empty() {
            hash.to_string()
        } else {
            format!("{}/{}", self.prefix.trim_end_matches('/'), hash)
        }
    }

    async fn ensure_bucket(&self) -> Result<()> {
        let url = self.bucket.head_bucket(Some(&self.creds)).sign(SIGN_TTL);
        let resp = self.http.head(url.as_str()).send().await.map_err(VcsError::storage)?;
        if resp.status().is_success() {
            return Ok(());
        }
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            let url = self.bucket.create_bucket(&self.creds).sign(SIGN_TTL);
            let put_resp = self.http.put(url.as_str()).send().await.map_err(VcsError::storage)?;
            if put_resp.status().is_success() || put_resp.status() == reqwest::StatusCode::CONFLICT
            {
                return Ok(());
            }
            return Err(VcsError::storage(format!(
                "could not create S3 bucket {}: {}",
                self.bucket.name(),
                put_resp.status()
            )));
        }
        Err(VcsError::storage(format!(
            "S3 bucket {} head failed: {}",
            self.bucket.name(),
            resp.status()
        )))
    }
}

impl BlobStore for S3Blobs {
    async fn put(&self, bytes: Vec<u8>) -> Result<Hash> {
        let h = sha256_hex(&bytes);
        if self.contains(&h).await? {
            return Ok(h);
        }
        let key = self.key(&h);
        let action = self.bucket.put_object(Some(&self.creds), &key);
        let url = action.sign(SIGN_TTL);
        let resp =
            self.http.put(url.as_str()).body(bytes).send().await.map_err(VcsError::storage)?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(VcsError::storage(format!("S3 PUT {key} failed: {status} {text}")));
        }
        Ok(h)
    }

    async fn get(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        let key = self.key(hash);
        let action = self.bucket.get_object(Some(&self.creds), &key);
        let url = action.sign(SIGN_TTL);
        let resp = self.http.get(url.as_str()).send().await.map_err(VcsError::storage)?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(VcsError::storage(format!("S3 GET {key} failed: {status} {text}")));
        }
        let bytes = resp.bytes().await.map_err(VcsError::storage)?;
        Ok(Some(bytes.to_vec()))
    }

    async fn contains(&self, hash: &str) -> Result<bool> {
        let key = self.key(hash);
        let action = self.bucket.head_object(Some(&self.creds), &key);
        let url = action.sign(SIGN_TTL);
        let resp = self.http.head(url.as_str()).send().await.map_err(VcsError::storage)?;
        if resp.status().is_success() {
            return Ok(true);
        }
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        Err(VcsError::storage(format!("S3 HEAD {key} failed: {}", resp.status())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_s3_blobs_roundtrip() {
        let cfg = S3Config {
            endpoint: "http://127.0.0.1:9000".into(),
            bucket: "holon-vcs-test-blobs".into(),
            region: "us-east-1".into(),
            access_key: "rustfsadmin".into(),
            secret_key: "rustfsadmin".into(),
            path_style: true,
            prefix: "test".into(),
        };
        let store = match S3Blobs::new(cfg).await {
            Ok(s) => s,
            Err(_) => {
                eprintln!("SKIPPED: local S3 store not reachable on :9000");
                return;
            }
        };

        let data = b"hello from s3 blobs adapter".to_vec();
        let hash = store.put(data.clone()).await.expect("put must succeed");
        assert!(store.contains(&hash).await.expect("contains check"));
        let retrieved = store.get(&hash).await.expect("get must succeed");
        assert_eq!(retrieved, Some(data));

        let missing = store
            .get("0000000000000000000000000000000000000000000000000000000000000000")
            .await
            .expect("missing get");
        assert_eq!(missing, None);
    }
}
