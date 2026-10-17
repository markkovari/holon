//! The S3-compatible bucket backups go to: R2, S3, RustFS, MinIO.
//!
//! rusty-s3 signs, reqwest sends — the `comp-media` shape. Files move in and
//! out without being held whole: a part under `PART` bytes is one PUT, anything
//! larger is a multipart upload of `PART`-sized pieces read one at a time, and
//! a download is written to disk as it arrives.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const SIGN_TTL: Duration = Duration::from_secs(3600);
/// Multipart piece size. S3 needs >= 5 MiB for every piece but the last.
const PART: usize = 64 << 20;

pub struct Store {
    http: reqwest::Client,
    bucket: Bucket,
    creds: Credentials,
}

#[derive(clap::Args, Debug, Clone)]
pub struct S3Args {
    /// S3 API endpoint, e.g. `https://<account>.r2.cloudflarestorage.com`.
    #[arg(long)]
    pub s3_endpoint: String,
    #[arg(long, default_value = "holon-backups")]
    pub s3_bucket: String,
    /// Key prefix every backup goes under.
    #[arg(long, default_value = "backups")]
    pub s3_prefix: String,
    /// R2 takes `auto`; RustFS and MinIO take `us-east-1`.
    #[arg(long, default_value = "us-east-1")]
    pub s3_region: String,
    #[arg(long)]
    pub s3_path_style: bool,
    /// Files holding the credentials; `S3_ACCESS_KEY` / `S3_SECRET_KEY` when absent.
    #[arg(long)]
    pub s3_access_key_file: Option<String>,
    #[arg(long)]
    pub s3_secret_key_file: Option<String>,
}

fn secret(file: &Option<String>, env: &str) -> Result<String> {
    match file {
        Some(p) => Ok(std::fs::read_to_string(p)
            .with_context(|| format!("reading {p}"))?
            .trim()
            .to_string()),
        None => std::env::var(env)
            .with_context(|| format!("no credentials file given and {env} is unset")),
    }
}

impl Store {
    pub async fn connect(a: &S3Args) -> Result<Self> {
        let style = if a.s3_path_style { UrlStyle::Path } else { UrlStyle::VirtualHost };
        let store = Self {
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(3600))
                .build()?,
            bucket: Bucket::new(
                a.s3_endpoint.parse().context("--s3-endpoint")?,
                style,
                a.s3_bucket.clone(),
                a.s3_region.clone(),
            )?,
            creds: Credentials::new(
                secret(&a.s3_access_key_file, "S3_ACCESS_KEY")?,
                secret(&a.s3_secret_key_file, "S3_SECRET_KEY")?,
            ),
        };
        store.ensure_bucket().await?;
        Ok(store)
    }

    async fn ensure_bucket(&self) -> Result<()> {
        let url = self.bucket.head_bucket(Some(&self.creds)).sign(SIGN_TTL);
        let status = self.http.head(url).send().await.context("HEAD bucket")?.status();
        if status.is_success() {
            return Ok(());
        }
        if status != reqwest::StatusCode::NOT_FOUND {
            bail!("S3 bucket {}: HTTP {status}", self.bucket.name());
        }
        let url = self.bucket.create_bucket(&self.creds).sign(SIGN_TTL);
        let resp = self.http.put(url).send().await.context("create bucket")?;
        let (status, body) = (resp.status(), resp.text().await.unwrap_or_default());
        if status.is_success() || body.contains("BucketAlreadyOwnedByYou") {
            return Ok(());
        }
        bail!("could not create S3 bucket {}: {status} {body}", self.bucket.name())
    }

    pub async fn put_bytes(&self, key: &str, body: Vec<u8>) -> Result<()> {
        let url = self.bucket.put_object(Some(&self.creds), key).sign(SIGN_TTL);
        check(self.http.put(url).body(body).send().await?, &format!("PUT {key}")).await.map(|_| ())
    }

    pub async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let url = self.bucket.get_object(Some(&self.creds), key).sign(SIGN_TTL);
        let resp = self.http.get(url).send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(check(resp, &format!("GET {key}")).await?.bytes().await?.to_vec()))
    }

    pub async fn put_file(&self, key: &str, path: &Path) -> Result<()> {
        let size = tokio::fs::metadata(path).await?.len() as usize;
        let mut file = tokio::fs::File::open(path).await?;
        if size <= PART {
            let mut body = Vec::with_capacity(size);
            file.read_to_end(&mut body).await?;
            return self.put_bytes(key, body).await;
        }
        let url = self.bucket.create_multipart_upload(Some(&self.creds), key).sign(SIGN_TTL);
        let body = check(self.http.post(url).send().await?, &format!("create upload {key}"))
            .await?
            .text()
            .await?;
        let upload = rusty_s3::actions::CreateMultipartUpload::parse_response(&body)
            .map_err(|e| anyhow::anyhow!("create upload {key}: {e}"))?;
        let id = upload.upload_id().to_string();
        let result: Result<()> = async {
            let mut etags = Vec::new();
            let mut number: u16 = 1;
            loop {
                let mut piece = Vec::with_capacity(PART);
                (&mut file).take(PART as u64).read_to_end(&mut piece).await?;
                if piece.is_empty() {
                    break;
                }
                let url =
                    self.bucket.upload_part(Some(&self.creds), key, number, &id).sign(SIGN_TTL);
                let resp = check(
                    self.http.put(url).body(piece).send().await?,
                    &format!("part {number} of {key}"),
                )
                .await?;
                let etag =
                    resp.headers().get("etag").and_then(|v| v.to_str().ok()).unwrap_or_default();
                etags.push(etag.to_string());
                number = number.checked_add(1).context("more than 65535 parts")?;
            }
            let action = self.bucket.complete_multipart_upload(
                Some(&self.creds),
                key,
                &id,
                etags.iter().map(String::as_str),
            );
            let url = action.sign(SIGN_TTL);
            let resp = self.http.post(url).body(action.body()).send().await?;
            let text = check(resp, &format!("complete upload {key}")).await?.text().await?;
            // CompleteMultipartUpload can answer 200 with an <Error> body.
            if text.contains("<Error>") {
                bail!("complete upload {key}: {text}");
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            let url =
                self.bucket.abort_multipart_upload(Some(&self.creds), key, &id).sign(SIGN_TTL);
            let _ = self.http.delete(url).send().await;
        }
        result
    }

    pub async fn get_file(&self, key: &str, path: &Path) -> Result<()> {
        let url = self.bucket.get_object(Some(&self.creds), key).sign(SIGN_TTL);
        let mut resp = check(self.http.get(url).send().await?, &format!("GET {key}")).await?;
        let mut file = tokio::fs::File::create(path).await?;
        while let Some(chunk) = resp.chunk().await? {
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        Ok(())
    }

    /// Every key under `prefix`, all pages.
    pub async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut action = self.bucket.list_objects_v2(Some(&self.creds));
            action.with_prefix(prefix);
            if let Some(t) = &token {
                action.with_continuation_token(t.as_str());
            }
            let body = check(
                self.http.get(action.sign(SIGN_TTL)).send().await?,
                &format!("LIST {prefix}"),
            )
            .await?
            .text()
            .await?;
            let page = rusty_s3::actions::ListObjectsV2::parse_response(&body)
                .map_err(|e| anyhow::anyhow!("LIST {prefix}: {e}"))?;
            keys.extend(page.contents.into_iter().map(|o| o.key));
            match page.next_continuation_token {
                Some(t) => token = Some(t),
                None => return Ok(keys),
            }
        }
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        let url = self.bucket.delete_object(Some(&self.creds), key).sign(SIGN_TTL);
        let resp = self.http.delete(url).send().await?;
        if resp.status().is_success() || resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        check(resp, &format!("DELETE {key}")).await.map(|_| ())
    }
}

async fn check(resp: reqwest::Response, what: &str) -> Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let code = body
        .split_once("<Code>")
        .and_then(|(_, r)| r.split_once("</Code>"))
        .map(|(c, _)| c.to_string())
        .unwrap_or_default();
    bail!("S3 {what}: HTTP {status} {code}")
}
