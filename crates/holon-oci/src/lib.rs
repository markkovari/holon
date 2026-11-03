//! Off the runtime path (nodes pull from the object store); for `wkg` interop. ADR-0017, ADR-0024.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use serde_json::json;

/// Must match what `wkg oci push` writes.
const MT_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const MT_CONFIG: &str = "application/vnd.wasm.config.v0+json";
const MT_LAYER: &str = "application/wasm";

/// Pull only: `wkg` writes `application/wasm`; wasmCloud and the OCI-wasm draft use the `vnd` forms.
const MT_WASM_LAYERS: &[&str] = &[
    MT_LAYER,
    "application/vnd.wasm.content.layer.v1+wasm",
    "application/vnd.module.wasm.content.layer.v1+wasm",
];

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

pub fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(bytes))
}

/// No registry crate: the media types must match `wkg` exactly. Returns the MANIFEST digest,
/// because a pull by digest resolves the manifest, not the layer.
pub async fn push_artifact(
    http: &reqwest::Client,
    base: &str,
    repo: &str,
    wasm: &[u8],
    exports: &[String],
    imports: &[String],
    creds: Option<&Creds>,
) -> Result<String> {
    let (config_bytes, manifest_bytes, manifest_digest, layer_digest) =
        oci_shape(wasm, exports, imports);
    upload_blob(http, base, repo, wasm, &layer_digest, creds).await?;
    upload_blob(http, base, repo, &config_bytes, &digest_of(&config_bytes), creds).await?;

    // A tag is human convenience only (ADR-0006).
    let tag = &layer_digest["sha256:".len()..][..12];
    let res = send(
        http,
        http.put(format!("{base}/v2/{repo}/manifests/{tag}"))
            .header("content-type", MT_MANIFEST)
            .body(manifest_bytes),
        creds,
        repo,
    )
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

#[derive(Clone, Debug, Default)]
pub struct Creds {
    pub user: String,
    pub pass: String,
}

impl Creds {
    /// `None`, not empty strings: an empty Basic header is rejected, none falls back to anonymous.
    pub fn from_env() -> Option<Self> {
        let user = std::env::var("OCI_USER").ok().filter(|v| !v.is_empty())?;
        let pass = std::env::var("OCI_PASSWORD").ok().filter(|v| !v.is_empty())?;
        Some(Creds { user, pass })
    }
}

/// Keyed by repository, not scope, so `pull` and `push,pull` share one. Without it every request
/// of a push re-fetched a token and pushing 82 apps timed the token endpoint out.
static TOKENS: std::sync::OnceLock<std::sync::Mutex<BTreeMap<String, String>>> =
    std::sync::OnceLock::new();

fn cached_token(repo: &str) -> Option<String> {
    TOKENS.get_or_init(Default::default).lock().ok()?.get(repo).cloned()
}

fn remember_token(repo: &str, token: &str) {
    if let Ok(mut m) = TOKENS.get_or_init(Default::default).lock() {
        m.insert(repo.to_string(), token.to_string());
    }
}

fn forget_token(repo: &str) {
    if let Ok(mut m) = TOKENS.get_or_init(Default::default).lock() {
        m.remove(repo);
    }
}

fn challenge_field(challenge: &str, key: &str) -> Option<String> {
    let at = challenge.find(&format!("{key}=\""))? + key.len() + 2;
    let rest = &challenge[at..];
    Some(rest[..rest.find('"')?].to_string())
}

async fn token_for(
    http: &reqwest::Client,
    challenge: &str,
    creds: Option<&Creds>,
) -> Result<String> {
    let realm = challenge_field(challenge, "realm")
        .with_context(|| format!("no realm in the auth challenge: {challenge:?}"))?;
    let mut req = http.get(&realm);
    if let Some(service) = challenge_field(challenge, "service") {
        req = req.query(&[("service", service)]);
    }
    if let Some(scope) = challenge_field(challenge, "scope") {
        req = req.query(&[("scope", scope)]);
    }
    if let Some(c) = creds {
        req = req.basic_auth(&c.user, Some(&c.pass));
    }
    let res = req.send().await.context("fetching a registry token")?;
    if !res.status().is_success() {
        bail!("the registry's token endpoint refused: {}", res.status());
    }
    let body: serde_json::Value = res.json().await.context("token response was not json")?;
    // Registries disagree on the field name.
    body["token"]
        .as_str()
        .or_else(|| body["access_token"].as_str())
        .map(str::to_string)
        .context("the token response carried no token")
}

/// Retries a 401 once, not in a loop: a second 401 means missing scope, and looping would hang.
async fn send(
    http: &reqwest::Client,
    req: reqwest::RequestBuilder,
    creds: Option<&Creds>,
    repo: &str,
) -> Result<reqwest::Response> {
    let retry = req.try_clone();
    let req = match cached_token(repo) {
        Some(t) => req.bearer_auth(t),
        None => req,
    };
    let res = req.send().await?;
    if res.status() != reqwest::StatusCode::UNAUTHORIZED {
        return Ok(res);
    }
    forget_token(repo);
    let challenge = res
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let Some(retry) = retry else { return Ok(res) };
    if challenge.is_empty() {
        return Ok(res);
    }
    // htpasswd registries challenge `Basic realm="Registry"`, a name not a URL: no token to fetch.
    if challenge.trim_start().to_ascii_lowercase().starts_with("basic") {
        let Some(c) = creds else {
            bail!("{repo}: the registry wants a username and password (OCI_USER / OCI_PASSWORD)");
        };
        return Ok(retry.basic_auth(&c.user, Some(&c.pass)).send().await?);
    }
    let token = token_for(http, &challenge, creds).await?;
    remember_token(repo, &token);
    Ok(retry.bearer_auth(token).send().await?)
}

/// The digest is the trust boundary (ADR-0024): the layer and a digest reference are both checked.
pub async fn pull_artifact(
    http: &reqwest::Client,
    base: &str,
    repo: &str,
    reference: &str,
    creds: Option<&Creds>,
) -> Result<Vec<u8>> {
    let res = send(
        http,
        http.get(format!("{base}/v2/{repo}/manifests/{reference}")).header("accept", MT_MANIFEST),
        creds,
        repo,
    )
    .await
    .context("GET manifest")?;
    if !res.status().is_success() {
        bail!("registry has no {repo}:{reference}: {}", res.status());
    }
    let manifest_bytes = res.bytes().await.context("reading the manifest")?.to_vec();

    if reference.starts_with("sha256:") {
        let got = digest_of(&manifest_bytes);
        if got != reference {
            bail!("asked for manifest {reference} and the registry served {got}");
        }
    }

    let manifest: serde_json::Value =
        serde_json::from_slice(&manifest_bytes).context("the manifest was not json")?;
    let layers = manifest["layers"].as_array().map(Vec::as_slice).unwrap_or_default();
    let layer = layers
        .iter()
        .find(|l| l["mediaType"].as_str().is_some_and(|mt| MT_WASM_LAYERS.contains(&mt)))
        .with_context(|| {
            // Name what it DID have; "not a component" alone sends people to their own build.
            let had: Vec<&str> = layers.iter().filter_map(|l| l["mediaType"].as_str()).collect();
            format!(
                "{repo}:{reference} carries no wasm layer — its layers are {had:?}, and a \
                 wasm one is any of {MT_WASM_LAYERS:?}"
            )
        })?;
    let digest = layer["digest"].as_str().context("the layer has no digest")?.to_string();

    let res = send(http, http.get(format!("{base}/v2/{repo}/blobs/{digest}")), creds, repo)
        .await
        .context("GET blob")?;
    if !res.status().is_success() {
        bail!("registry refused the layer {digest}: {}", res.status());
    }
    let wasm = res.bytes().await.context("reading the layer")?.to_vec();

    let got = digest_of(&wasm);
    if got != digest {
        bail!("{repo}:{reference} layer is {digest} and the bytes hash to {got}");
    }
    Ok(wasm)
}

/// `(config, manifest, manifest digest, layer digest)`.
pub fn oci_shape(
    wasm: &[u8],
    exports: &[String],
    imports: &[String],
) -> (Vec<u8>, Vec<u8>, String, String) {
    let layer_digest = digest_of(wasm);

    // Fixed, not wall-clock: `created` is in the config blob, so a real time would mint new digests
    // on every push; pinned, a retry is a no-op.
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
    creds: Option<&Creds>,
) -> Result<()> {
    // Blobs are content-addressed, so skipping an existing one is safe and keeps a retry cheap.
    if let Ok(r) =
        send(http, http.head(format!("{base}/v2/{repo}/blobs/{digest}")), creds, repo).await
    {
        if r.status().is_success() {
            return Ok(());
        }
    }
    let start = send(
        http,
        http.post(format!("{base}/v2/{repo}/blobs/uploads/")).header("content-length", "0"),
        creds,
        repo,
    )
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
    let res = send(
        http,
        http.put(format!("{url}{sep}digest={digest}"))
            .header("content-type", "application/octet-stream")
            .body(bytes.to_vec()),
        creds,
        repo,
    )
    .await
    .context("PUT blob")?;
    if !res.status().is_success() {
        bail!("registry refused a blob: {} {}", res.status(), res.text().await.unwrap_or_default());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_token_is_remembered_per_repository() {
        use super::{cached_token, forget_token, remember_token};
        // Unique names: the cache is process-wide, so collisions would be order-dependent.
        let (a, b) = ("owner/holon-apps/events", "owner/holon-apps/poll");
        assert_eq!(cached_token(a), None, "nothing is cached before anything is put");

        remember_token(a, "tok-a");
        remember_token(b, "tok-b");
        assert_eq!(cached_token(a).as_deref(), Some("tok-a"));
        assert_eq!(cached_token(b).as_deref(), Some("tok-b"));

        forget_token(a);
        assert_eq!(cached_token(a), None);
        assert_eq!(cached_token(b).as_deref(), Some("tok-b"), "the other survives");
        forget_token(b);
    }

    use super::*;

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

        let (_, _, again, _) = oci_shape(
            wasm,
            &["wasi:http/incoming-handler@0.2.0".to_string()],
            &["wasi:keyvalue/store@0.2.0-draft".to_string()],
        );
        assert_eq!(manifest_digest, again, "the push must be a pure function of the bytes");

        assert_ne!(manifest_digest, layer_digest);
    }

    #[tokio::test]
    async fn pulls_back_exactly_what_was_pushed_and_refuses_anything_else() {
        use axum::body::Bytes;
        use axum::http::{Method, StatusCode, Uri};
        use axum::Router;
        use std::collections::HashMap;
        use std::sync::Mutex;

        static BLOBS: Mutex<Option<HashMap<String, Vec<u8>>>> = Mutex::new(None);
        static TAGS: Mutex<Option<HashMap<String, Vec<u8>>>> = Mutex::new(None);
        static TAMPER: Mutex<bool> = Mutex::new(false);
        *BLOBS.lock().unwrap() = Some(HashMap::new());
        *TAGS.lock().unwrap() = Some(HashMap::new());

        let app = Router::new().fallback(|method: Method, uri: Uri, body: Bytes| async move {
            let path = uri.path().to_string();
            let query = uri.query().unwrap_or_default().to_string();

            if method == Method::POST && path.ends_with("/blobs/uploads/") {
                return (StatusCode::ACCEPTED, [("location", "/upload/s".to_string())], Vec::new());
            }
            if method == Method::PUT && path == "/upload/s" {
                let digest = query
                    .split('&')
                    .filter_map(|kv| kv.split_once('='))
                    .find(|(k, _)| *k == "digest")
                    .map(|(_, v)| v.to_string())
                    .unwrap_or_default();
                BLOBS.lock().unwrap().as_mut().unwrap().insert(digest, body.to_vec());
                return (StatusCode::CREATED, [("location", String::new())], Vec::new());
            }
            if method == Method::PUT && path.contains("/manifests/") {
                let tag = path.rsplit("/manifests/").next().unwrap_or_default().to_string();
                let bytes = body.to_vec();
                // A real registry addresses a manifest by its digest too.
                TAGS.lock().unwrap().as_mut().unwrap().insert(digest_of(&bytes), bytes.clone());
                TAGS.lock().unwrap().as_mut().unwrap().insert(tag, bytes);
                return (StatusCode::CREATED, [("location", String::new())], Vec::new());
            }
            if method == Method::GET && path.contains("/manifests/") {
                let tag = path.rsplit("/manifests/").next().unwrap_or_default().to_string();
                return match TAGS.lock().unwrap().as_ref().unwrap().get(&tag) {
                    Some(m) => (StatusCode::OK, [("location", String::new())], m.clone()),
                    None => (StatusCode::NOT_FOUND, [("location", String::new())], Vec::new()),
                };
            }
            if method == Method::GET && path.contains("/blobs/") {
                let digest = path.rsplit("/blobs/").next().unwrap_or_default().to_string();
                return match BLOBS.lock().unwrap().as_ref().unwrap().get(&digest) {
                    Some(b) => {
                        let mut b = b.clone();
                        if *TAMPER.lock().unwrap() {
                            b.push(b'!');
                        }
                        (StatusCode::OK, [("location", String::new())], b)
                    }
                    None => (StatusCode::NOT_FOUND, [("location", String::new())], Vec::new()),
                };
            }
            (StatusCode::NOT_FOUND, [("location", String::new())], Vec::new())
        });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base = format!("http://{addr}");
        let http = reqwest::Client::new();

        let wasm = b"\0asm\x0d\0\0\0 a component worth fetching".to_vec();
        let manifest_digest =
            push_artifact(&http, &base, "acme/api", &wasm, &[], &[], None).await.expect("push");

        let tag = digest_of(&wasm)["sha256:".len()..][..12].to_string();
        let got = pull_artifact(&http, &base, "acme/api", &tag, None).await.expect("pull by tag");
        assert_eq!(got, wasm, "the bytes must survive the round trip");

        let got = pull_artifact(&http, &base, "acme/api", &manifest_digest, None)
            .await
            .expect("pull by digest");
        assert_eq!(got, wasm);

        assert!(pull_artifact(&http, &base, "acme/api", "nope", None).await.is_err());

        *TAMPER.lock().unwrap() = true;
        let err = pull_artifact(&http, &base, "acme/api", &tag, None)
            .await
            .expect_err("a layer that does not match its digest must be refused");
        assert!(
            format!("{err:#}").contains("hash to"),
            "the error should say the bytes do not match: {err:#}"
        );
    }

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

        // One handler: OCI repo names contain slashes and axum takes no wildcard mid-route.
        let app = Router::new().fallback(
            |method: axum::http::Method, uri: axum::http::Uri, body: axum::body::Bytes| async move {
                let path = uri.path().to_string();
                let query = uri.query().unwrap_or_default().to_string();
                if method == axum::http::Method::POST && path.ends_with("/blobs/uploads/") {
                    // Relative on purpose: the form a naive client mishandles.
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
                // Including the HEAD probe, so every blob is a fresh upload.
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
            None,
        )
        .await
        .expect("push");

        let seen = SEEN.lock().unwrap().take().unwrap();
        assert_eq!(seen.blobs.len(), 2, "the layer and the config: {:?}", seen.blobs);
        assert!(seen.blobs.iter().any(|(_, len)| *len == wasm.len()), "the wasm itself");
        let (reference, put_digest) = seen.manifest.expect("a manifest was PUT");
        assert_eq!(digest, put_digest);
        assert_eq!(reference, digest_of(&wasm)["sha256:".len()..][..12]);
    }
}
