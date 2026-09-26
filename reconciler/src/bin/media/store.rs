//! The S3-compatible store: buckets, presigned URLs, and turning a non-2xx
//! answer into the contract's error shape.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusty_s3::{Bucket, Credentials, S3Action};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::errors::{unavailable, MediaError};
use crate::keys::Which;

/// Part URLs live this long — enough for a slow uplink to push 512 MB.
pub(crate) const UPLOAD_URL_TTL: Duration = Duration::from_secs(6 * 3600);

pub(crate) struct Store {
    pub(crate) http: reqwest::Client,
    pub(crate) creds: Credentials,
    pub(crate) originals: Bucket,
    pub(crate) renditions: Bucket,
    /// The same buckets at `--s3-public-endpoint`: what a browser is handed.
    pub(crate) public_originals: Bucket,
    pub(crate) public_renditions: Bucket,
}

impl Store {
    pub(crate) fn bucket(&self, w: Which) -> &Bucket {
        match w {
            Which::Originals => &self.originals,
            Which::Renditions => &self.renditions,
        }
    }

    fn public_bucket(&self, w: Which) -> &Bucket {
        match w {
            Which::Originals => &self.public_originals,
            Which::Renditions => &self.public_renditions,
        }
    }

    /// Turn a non-2xx store answer into the contract's error. S3 puts the
    /// reason in an XML `<Code>`; a missing upload or key is `not-found`, a bad
    /// part list is `refused` (the caller sent it), anything else is ours.
    async fn check(resp: reqwest::Response, what: &str) -> Result<String, MediaError> {
        let status = resp.status();
        let body = resp.text().await.map_err(unavailable)?;
        // CompleteMultipartUpload can answer 200 with an <Error> body.
        if status.is_success() && !body.contains("<Error>") {
            return Ok(body);
        }
        let code = xml_tag(&body, "Code").unwrap_or_default();
        let detail = format!("{what}: {status} {code}");
        Err(match code.as_str() {
            "NoSuchUpload" | "NoSuchKey" | "NoSuchBucket" => MediaError::NotFound(detail),
            "InvalidPart" | "InvalidPartOrder" | "EntityTooSmall" => MediaError::Refused(detail),
            _ if status == reqwest::StatusCode::NOT_FOUND => MediaError::NotFound(detail),
            _ => MediaError::Unavailable(detail),
        })
    }

    pub(crate) async fn ensure_bucket(&self, w: Which) -> Result<()> {
        let b = self.bucket(w);
        let url = b.head_bucket(Some(&self.creds)).sign(Duration::from_secs(60));
        let resp = self.http.head(url).send().await.context("HEAD bucket")?;
        if resp.status().is_success() {
            return Ok(());
        }
        let url = b.create_bucket(&self.creds).sign(Duration::from_secs(60));
        let resp = self.http.put(url).send().await.context("create bucket")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        // A race with another instance creating it is fine.
        if status.is_success() || body.contains("BucketAlreadyOwnedByYou") {
            eprintln!("comp-media: created bucket {}", b.name());
            return Ok(());
        }
        bail!("could not create bucket {}: {status} {body}", b.name())
    }

    /// PutBucketCors, signed as a `PUT ?cors` on the bucket — rusty-s3 has no
    /// action for it, and `CreateBucket` is exactly a signed `PUT` on the
    /// bucket URL, so the query is the only difference. `ETag` must be exposed
    /// or the browser cannot read the part's tag back.
    pub(crate) async fn put_cors(&self, w: Which, origins: &[String]) -> Result<()> {
        use base64::Engine as _;
        let b = self.bucket(w);
        let body = cors_xml(origins);
        let md5 =
            base64::engine::general_purpose::STANDARD.encode(md5::Md5::digest(body.as_bytes()));
        let mut action = b.create_bucket(&self.creds);
        action.query_mut().insert("cors", "");
        let url = action.sign(Duration::from_secs(60));
        let resp = self.http.put(url).header("Content-MD5", md5).body(body).send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let text = resp.text().await.unwrap_or_default();
        bail!("{status} {}", xml_tag(&text, "Code").unwrap_or(text))
    }

    pub(crate) async fn healthy(&self) -> bool {
        let url = self.originals.head_bucket(Some(&self.creds)).sign(Duration::from_secs(30));
        matches!(
            self.http.head(url).timeout(Duration::from_secs(3)).send().await,
            Ok(r) if r.status().is_success()
        )
    }

    pub(crate) async fn create_upload(
        &self,
        object: &str,
        content_type: &str,
    ) -> Result<String, MediaError> {
        let url = self
            .originals
            .create_multipart_upload(Some(&self.creds), object)
            .sign(Duration::from_secs(60));
        let mut req = self.http.post(url);
        if !content_type.is_empty() {
            req = req.header("Content-Type", content_type);
        }
        let body = Self::check(req.send().await.map_err(unavailable)?, "create upload").await?;
        let parsed =
            rusty_s3::actions::CreateMultipartUpload::parse_response(&body).map_err(unavailable)?;
        Ok(parsed.upload_id().to_string())
    }

    pub(crate) fn part_url(&self, object: &str, number: u16, upload_id: &str) -> String {
        self.public_originals
            .upload_part(Some(&self.creds), object, number, upload_id)
            .sign(UPLOAD_URL_TTL)
            .to_string()
    }

    pub(crate) async fn complete_upload(
        &self,
        object: &str,
        upload_id: &str,
        etags: &[String],
    ) -> Result<(), MediaError> {
        let action = self.originals.complete_multipart_upload(
            Some(&self.creds),
            object,
            upload_id,
            etags.iter().map(String::as_str),
        );
        let url = action.sign(Duration::from_secs(60));
        let body = action.body();
        Self::check(
            self.http.post(url).body(body).send().await.map_err(unavailable)?,
            "complete upload",
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn abort_upload(
        &self,
        object: &str,
        upload_id: &str,
    ) -> Result<(), MediaError> {
        let url = self
            .originals
            .abort_multipart_upload(Some(&self.creds), object, upload_id)
            .sign(Duration::from_secs(60));
        Self::check(self.http.delete(url).send().await.map_err(unavailable)?, "abort upload")
            .await?;
        Ok(())
    }

    pub(crate) fn sign_get(&self, w: Which, object: &str, ttl: Duration) -> String {
        self.public_bucket(w).get_object(Some(&self.creds), object).sign(ttl).to_string()
    }

    pub(crate) async fn put(
        &self,
        w: Which,
        object: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<()> {
        let url =
            self.bucket(w).put_object(Some(&self.creds), object).sign(Duration::from_secs(300));
        let resp =
            self.http.put(url).header("Content-Type", content_type).body(bytes).send().await?;
        Self::check(resp, "put object").await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
        Ok(())
    }

    /// Stream an object to `dest`, hashing as it goes — the 129 MB original is
    /// never in memory, let alone twice. Returns (bytes, sha256 hex).
    pub(crate) async fn download(
        &self,
        w: Which,
        object: &str,
        dest: &Path,
        max: u64,
    ) -> Result<(u64, String)> {
        let url =
            self.bucket(w).get_object(Some(&self.creds), object).sign(Duration::from_secs(600));
        let mut resp = self.http.get(url).send().await?;
        if !resp.status().is_success() {
            let s = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("GET {object}: {s} {}", xml_tag(&text, "Code").unwrap_or_default());
        }
        let mut file =
            tokio::io::BufWriter::with_capacity(1 << 20, tokio::fs::File::create(dest).await?);
        let mut hasher = Sha256::new();
        let mut total = 0u64;
        while let Some(chunk) = resp.chunk().await? {
            total += chunk.len() as u64;
            if total > max {
                bail!("{object} is larger than --max-upload-mb");
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        Ok((total, hex(&hasher.finalize())))
    }
}

/// The text of the first `<tag>…</tag>` — enough XML for an S3 error `Code`.
pub(crate) fn xml_tag(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&format!("</{tag}>"))? + start;
    Some(body[start..end].to_string())
}

fn cors_xml(origins: &[String]) -> String {
    let origins: String = origins
        .iter()
        .map(|o| format!("<AllowedOrigin>{}</AllowedOrigin>", xml_escape(o)))
        .collect();
    format!(
        "<CORSConfiguration><CORSRule>{origins}<AllowedMethod>PUT</AllowedMethod><AllowedMethod>GET</AllowedMethod>\
         <AllowedMethod>HEAD</AllowedMethod><AllowedHeader>*</AllowedHeader><ExposeHeader>ETag</ExposeHeader>\
         <MaxAgeSeconds>3600</MaxAgeSeconds></CORSRule></CORSConfiguration>"
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// `url` is http(s) and its `host:port` (port defaulted from the scheme) is
/// on `allow`, compared case-insensitively. An empty list allows nothing.
pub(crate) fn callback_allowed(url: &str, allow: &[String]) -> Result<(), String> {
    let u = reqwest::Url::parse(url).map_err(|_| format!("callback_url is not a URL: {url:?}"))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(format!("callback_url must be http(s): {url:?}"));
    }
    let (Some(host), Some(port)) = (u.host_str(), u.port_or_known_default()) else {
        return Err(format!("callback_url has no host: {url:?}"));
    };
    let authority = format!("{host}:{port}").to_ascii_lowercase();
    if allow.iter().any(|a| a.trim().eq_ignore_ascii_case(&authority)) {
        Ok(())
    } else {
        Err(format!("callback_url {authority} is not on --callback-allow"))
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn now_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use rusty_s3::UrlStyle;

    use super::*;

    #[test]
    fn s3_error_codes_are_read_from_the_xml() {
        let body =
            "<?xml version=\"1.0\"?><Error><Code>NoSuchUpload</Code><Message>x</Message></Error>";
        assert_eq!(xml_tag(body, "Code").as_deref(), Some("NoSuchUpload"));
        assert_eq!(xml_tag("<a/>", "Code"), None);
    }

    /// Only an allow-listed `host:port` is a callback target; the port comes
    /// from the scheme when absent, and an empty list refuses everything.
    #[test]
    fn a_callback_must_be_on_the_allow_list() {
        let allow = vec!["127.0.0.1:3941".to_string(), "App.Example.com:443".to_string()];
        assert!(
            callback_allowed("http://127.0.0.1:3941/internal/photos/x/evaluated", &allow).is_ok()
        );
        assert!(callback_allowed("https://app.example.com/cb", &allow).is_ok());
        for bad in [
            "http://127.0.0.1:3942/cb",
            "http://169.254.169.254/latest/meta-data",
            "http://localhost:3941/cb",
            "http://app.example.com/cb", // port 80, not 443
            "ftp://127.0.0.1:3941/cb",
            "not a url",
        ] {
            assert!(callback_allowed(bad, &allow).is_err(), "{bad:?} must be refused");
        }
        assert!(callback_allowed("http://127.0.0.1:3941/cb", &[]).is_err());
    }

    /// Browsers get URLs signed for the public endpoint; the daemon's own
    /// calls use the internal one.
    #[test]
    fn browser_urls_are_signed_against_the_public_endpoint() {
        let b = |e: &str, n: &str| {
            Bucket::new(e.parse().unwrap(), UrlStyle::Path, n.to_string(), "us-east-1".to_string())
                .unwrap()
        };
        let store = Store {
            http: reqwest::Client::new(),
            creds: Credentials::new("k", "s"),
            originals: b("http://rustfs:9000", "originals"),
            renditions: b("http://rustfs:9000", "renditions"),
            public_originals: b("https://s3.example.com", "originals"),
            public_renditions: b("https://s3.example.com", "renditions"),
        };
        let part = store.part_url("p1.arw", 1, "u1");
        assert!(part.starts_with("https://s3.example.com/originals/p1.arw?"), "{part}");
        let get = store.sign_get(Which::Renditions, "p1/share.jpg", Duration::from_secs(60));
        assert!(get.starts_with("https://s3.example.com/renditions/p1/share.jpg?"), "{get}");
        assert!(store
            .bucket(Which::Originals)
            .base_url()
            .as_str()
            .starts_with("http://rustfs:9000"));
    }
}
