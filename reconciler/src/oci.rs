//! OCI push, as salvaged from the applier (ADR-0017).
//!
//! No longer on the runtime path — artifacts reach nodes through the JetStream
//! object store, keyed by their own digest, so a node needs no registry and no
//! registry credential. This survives behind `--oci-mirror` because it was proven
//! against a real registry and because `wkg oci pull` interop is worth keeping
//! cheap. Deleting it would save nothing and cost a rewrite the first time someone
//! wants it back.

use anyhow::{bail, Context, Result};
use serde_json::json;

/// Media types matched to what `wkg oci push` writes — read off a real artifact in
/// a running registry rather than guessed, because whoever pulls this has to be
/// able to.
const MT_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const MT_CONFIG: &str = "application/vnd.wasm.config.v0+json";
const MT_LAYER: &str = "application/wasm";

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

pub fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(bytes))
}

/// Push one component, by hand, over the OCI distribution API.
///
/// Four calls and no registry crate: start an upload, PUT the layer, PUT the
/// config, PUT the manifest. Written out rather than taken as a dependency because
/// the **media types have to match `wkg` exactly**, and an abstraction that picks
/// them for us is the one thing we do not want here.
///
/// Returns the **manifest** digest. That distinction matters: a pull by digest
/// resolves the manifest, not the layer, so using the wasm's own hash would produce
/// a reference that never resolves.
pub async fn push_artifact(
    http: &reqwest::Client,
    base: &str,
    repo: &str,
    wasm: &[u8],
    exports: &[String],
    imports: &[String],
) -> Result<String> {
    let (config_bytes, manifest_bytes, manifest_digest, layer_digest) =
        oci_shape(wasm, exports, imports);
    upload_blob(http, base, repo, wasm, &layer_digest).await?;
    upload_blob(http, base, repo, &config_bytes, &digest_of(&config_bytes)).await?;

    // Tagged with the artifact's own content hash, short. A tag is human
    // convenience only (ADR-0006) — nothing is ever deployed by one — and a
    // content-derived tag can never change meaning under someone.
    let tag = &layer_digest["sha256:".len()..][..12];
    let res = http
        .put(format!("{base}/v2/{repo}/manifests/{tag}"))
        .header("content-type", MT_MANIFEST)
        .body(manifest_bytes)
        .send()
        .await
        .context("PUT manifest")?;
    if !res.status().is_success() {
        bail!(
            "registry refused the manifest: {} {}",
            res.status(),
            res.text().await.unwrap_or_default()
        );
    }
    Ok(manifest_digest)
}

/// The bytes an OCI wasm artifact is made of: `(config, manifest, manifest digest,
/// layer digest)`.
///
/// Pure, so the shape can be asserted against a real `wkg`-produced artifact
/// without a registry — the test that matters, since a wrong media type produces
/// something nothing can pull.
pub fn oci_shape(
    wasm: &[u8],
    exports: &[String],
    imports: &[String],
) -> (Vec<u8>, Vec<u8>, String, String) {
    let layer_digest = digest_of(wasm);

    // A FIXED timestamp, deliberately. `created` is part of the config blob, so a
    // wall-clock value there would change the config digest, which changes the
    // manifest digest — meaning the same bytes would push to a different reference
    // every time and a re-push would mint a second identity for one artifact.
    // Pinning it makes the whole push a pure function of the component: same bytes,
    // same digest, and a retry is a no-op the registry deduplicates.
    let config = json!({
        "created": "1970-01-01T00:00:00Z",
        "author": null,
        "architecture": "wasm",
        "os": "wasip2",
        "layerDigests": [layer_digest],
        "component": { "exports": exports, "imports": imports, "target": null },
    });
    let config_bytes = serde_json::to_vec(&config).expect("config json");
    let config_digest = digest_of(&config_bytes);

    let manifest = json!({
        "schemaVersion": 2,
        "mediaType": MT_MANIFEST,
        "config": { "mediaType": MT_CONFIG, "digest": config_digest, "size": config_bytes.len() },
        "layers": [{ "mediaType": MT_LAYER, "digest": layer_digest, "size": wasm.len() }],
    });
    let manifest_bytes = serde_json::to_vec(&manifest).expect("manifest json");
    let manifest_digest = digest_of(&manifest_bytes);
    (config_bytes, manifest_bytes, manifest_digest, layer_digest)
}

async fn upload_blob(
    http: &reqwest::Client,
    base: &str,
    repo: &str,
    bytes: &[u8],
    digest: &str,
) -> Result<()> {
    // Already there? Blobs are content-addressed, so skipping is always safe, and
    // it makes a retried push cheap instead of re-sending the whole component.
    if let Ok(r) = http.head(format!("{base}/v2/{repo}/blobs/{digest}")).send().await {
        if r.status().is_success() {
            return Ok(());
        }
    }
    let start = http
        .post(format!("{base}/v2/{repo}/blobs/uploads/"))
        .header("content-length", "0")
        .send()
        .await
        .context("starting a blob upload")?;
    if !start.status().is_success() {
        bail!("registry refused an upload session: {}", start.status());
    }
    let location = start
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .context("upload session has no Location")?
        .to_string();
    // Location may be absolute or root-relative; both are legal.
    let url = if location.starts_with("http") { location } else { format!("{base}{location}") };
    let sep = if url.contains('?') { '&' } else { '?' };
    let res = http
        .put(format!("{url}{sep}digest={digest}"))
        .header("content-type", "application/octet-stream")
        .body(bytes.to_vec())
        .send()
        .await
        .context("PUT blob")?;
    if !res.status().is_success() {
        bail!("registry refused a blob: {} {}", res.status(), res.text().await.unwrap_or_default());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The artifact shape, asserted against a REAL `wkg oci push` artifact read out
    /// of a running registry. If any of these drift, whoever pulls gets something
    /// they cannot use — and it fails on someone else's app, not here.
    #[test]
    fn the_oci_shape_matches_what_wkg_writes() {
        let wasm = b"\0asm\x0d\0\0\0 pretend component";
        let (config, manifest, manifest_digest, layer_digest) = oci_shape(
            wasm,
            &["wasi:http/incoming-handler@0.2.0".to_string()],
            &["wasi:keyvalue/store@0.2.0-draft".to_string()],
        );
        let m: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
        assert_eq!(m["mediaType"], "application/vnd.oci.image.manifest.v1+json");
        assert_eq!(m["schemaVersion"], 2);
        assert_eq!(m["config"]["mediaType"], "application/vnd.wasm.config.v0+json");
        assert_eq!(m["layers"][0]["mediaType"], "application/wasm");
        assert_eq!(m["layers"][0]["size"], wasm.len());
        assert_eq!(m["layers"][0]["digest"], layer_digest);
        assert_eq!(m["config"]["digest"], digest_of(&config));

        let c: serde_json::Value = serde_json::from_slice(&config).unwrap();
        assert_eq!(c["architecture"], "wasm");
        assert_eq!(c["os"], "wasip2");
        assert_eq!(c["layerDigests"][0], layer_digest);
        assert_eq!(c["component"]["exports"][0], "wasi:http/incoming-handler@0.2.0");
        assert!(c["component"]["target"].is_null());

        // Same bytes, same digest — always. A wall-clock `created` here would mint a
        // new identity for one artifact on every retry.
        let (_, _, again, _) = oci_shape(
            wasm,
            &["wasi:http/incoming-handler@0.2.0".to_string()],
            &["wasi:keyvalue/store@0.2.0-draft".to_string()],
        );
        assert_eq!(manifest_digest, again, "the push must be a pure function of the bytes");

        // What gets pinned is the MANIFEST digest, not the layer's. Getting this
        // wrong yields a reference that never resolves.
        assert_ne!(manifest_digest, layer_digest);
    }

    /// The four-call upload dance, against a registry that records what it is sent.
    /// This is the half that cannot be checked by inspecting JSON: the upload
    /// session, the `?digest=` parameter, and a relative vs absolute `Location`.
    #[tokio::test]
    async fn pushes_blobs_then_the_manifest() {
        use axum::http::StatusCode;
        use axum::Router;
        use std::sync::Mutex;

        #[derive(Default)]
        struct Seen {
            blobs: Vec<(String, usize)>,
            manifest: Option<(String, String)>,
        }
        static SEEN: Mutex<Option<Seen>> = Mutex::new(None);
        *SEEN.lock().unwrap() = Some(Seen::default());

        // One handler dispatching on method+path, because OCI repo names contain
        // slashes and axum will not take a wildcard mid-route. Closer to a real
        // registry's routing anyway.
        let app = Router::new().fallback(
            |method: axum::http::Method, uri: axum::http::Uri, body: axum::body::Bytes| async move {
                let path = uri.path().to_string();
                let query = uri.query().unwrap_or_default().to_string();
                if method == axum::http::Method::POST && path.ends_with("/blobs/uploads/") {
                    // Relative Location on purpose: both forms are legal and this is
                    // the one a naive client mishandles.
                    return (StatusCode::ACCEPTED, [("location", "/upload/session-1".to_string())]);
                }
                if method == axum::http::Method::PUT && path == "/upload/session-1" {
                    let digest = query
                        .split('&')
                        .filter_map(|kv| kv.split_once('='))
                        .find(|(k, _)| *k == "digest")
                        .map(|(_, v)| v.to_string())
                        .unwrap_or_default();
                    assert!(digest.starts_with("sha256:"), "no digest param in {query:?}");
                    assert_eq!(digest, digest_of(&body), "the digest must describe the bytes");
                    SEEN.lock().unwrap().as_mut().unwrap().blobs.push((digest, body.len()));
                    return (StatusCode::CREATED, [("location", String::new())]);
                }
                if method == axum::http::Method::PUT && path.contains("/manifests/") {
                    let reference =
                        path.rsplit("/manifests/").next().unwrap_or_default().to_string();
                    SEEN.lock().unwrap().as_mut().unwrap().manifest =
                        Some((reference, digest_of(&body)));
                    return (StatusCode::CREATED, [("location", String::new())]);
                }
                // Anything else, including the HEAD cache probe: not found, so every
                // blob is a fresh upload in this test.
                (StatusCode::NOT_FOUND, [("location", String::new())])
            },
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let wasm = b"\0asm\x0d\0\0\0 a component".to_vec();
        let http = reqwest::Client::new();
        let digest = push_artifact(
            &http,
            &format!("http://{addr}"),
            "acme/api",
            &wasm,
            &["wasi:http/incoming-handler@0.2.0".to_string()],
            &[],
        )
        .await
        .expect("push");

        let seen = SEEN.lock().unwrap().take().unwrap();
        assert_eq!(seen.blobs.len(), 2, "the layer and the config: {:?}", seen.blobs);
        assert!(seen.blobs.iter().any(|(_, len)| *len == wasm.len()), "the wasm itself");
        let (reference, put_digest) = seen.manifest.expect("a manifest was PUT");
        // What we return must be the digest of what we actually sent, or the catalog
        // records a reference the registry cannot resolve.
        assert_eq!(digest, put_digest);
        // Tagged by content, so a tag can never change meaning under someone.
        assert_eq!(reference, digest_of(&wasm)["sha256:".len()..][..12]);
    }
}
