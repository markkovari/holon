//! IdP-agnostic OIDC client: discovery, JWKS fetch/cache, code exchange.
//! All outbound HTTP goes through `wasi:http/outgoing-handler`.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;

use crate::bindings::exports::auth::identity::types::{AuthError, TokenPair};
use crate::bindings::exports::auth::identity::oidc::OidcConfig;
use crate::bindings::wasi::http::outgoing_handler;
use crate::bindings::wasi::http::types::{
    Fields, Method, OutgoingRequest, RequestOptions, Scheme,
};
use crate::bindings::wasi::clocks::wall_clock;
use crate::bindings::wasi::io::streams::StreamError;
use crate::{config, kv};

/// Read a TTL-cached value: entries are stored as `<expiry-epoch>:<payload>`.
/// Returns the payload if present and unexpired.
fn cache_get(key: &str) -> Option<String> {
    let raw = kv::get(key).ok().flatten()?;
    let (exp, payload) = raw.split_once(':')?;
    let exp: u64 = exp.parse().ok()?;
    if exp > wall_clock::now().seconds {
        Some(payload.to_string())
    } else {
        None
    }
}

/// Store a value with `jwks-cache-ttl` seconds of freshness.
fn cache_put(key: &str, payload: &str) {
    let exp = wall_clock::now().seconds + config::jwks_cache_ttl();
    let _ = kv::set(key, &format!("{exp}:{payload}"));
}

#[derive(Deserialize)]
struct DiscoveryDoc {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    #[serde(default)]
    userinfo_endpoint: Option<String>,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kty: String,
    #[serde(default)]
    kid: Option<String>,
    // RSA
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    // EC
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
}

/// Fetch (and cache) the issuer's discovery document.
pub fn discover(issuer: &str) -> Result<OidcConfig, AuthError> {
    let doc = discovery(issuer)?;
    Ok(OidcConfig {
        issuer: doc.issuer,
        authorization_endpoint: doc.authorization_endpoint,
        token_endpoint: doc.token_endpoint,
        jwks_uri: doc.jwks_uri,
        userinfo_endpoint: doc.userinfo_endpoint,
    })
}

fn discovery(issuer: &str) -> Result<DiscoveryDoc, AuthError> {
    let cache_key = format!("oidc:{issuer}");
    if let Some(body) = cache_get(&cache_key) {
        if let Ok(doc) = serde_json::from_str::<DiscoveryDoc>(&body) {
            return Ok(doc);
        }
    }
    let url = format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/'));
    let body = http_get(&url)?;
    let doc: DiscoveryDoc = serde_json::from_slice(&body)
        .map_err(|e| AuthError::BackendUnavailable(format!("discovery parse: {e}")))?;
    cache_put(&cache_key, &String::from_utf8_lossy(&body));
    Ok(doc)
}

fn fetch_jwks(issuer: &str) -> Result<Jwks, AuthError> {
    let cache_key = format!("jwks:{issuer}");
    if let Some(body) = cache_get(&cache_key) {
        if let Ok(jwks) = serde_json::from_str::<Jwks>(&body) {
            return Ok(jwks);
        }
    }
    let doc = discovery(issuer)?;
    let body = http_get(&doc.jwks_uri)?;
    let jwks: Jwks = serde_json::from_slice(&body)
        .map_err(|e| AuthError::BackendUnavailable(format!("jwks parse: {e}")))?;
    cache_put(&cache_key, &String::from_utf8_lossy(&body));
    Ok(jwks)
}

fn select_key<'a>(jwks: &'a Jwks, kid: Option<&str>) -> Option<&'a Jwk> {
    match kid {
        Some(k) => jwks.keys.iter().find(|j| j.kid.as_deref() == Some(k)),
        None => jwks.keys.first(),
    }
}

/// Resolve the RSA modulus (n) and exponent (e) for the issuer/kid.
pub fn jwks_rsa_key(issuer: &str, kid: Option<&str>) -> Result<(Vec<u8>, Vec<u8>), AuthError> {
    let jwks = fetch_jwks(issuer)?;
    let jwk = select_key(&jwks, kid)
        .ok_or_else(|| AuthError::InvalidToken("no matching jwk".into()))?;
    if jwk.kty != "RSA" {
        return Err(AuthError::InvalidToken("jwk is not RSA".into()));
    }
    let n = decode_b64(jwk.n.as_deref())?;
    let e = decode_b64(jwk.e.as_deref())?;
    Ok((n, e))
}

/// Resolve the EC public point coordinates (x, y) for the issuer/kid.
pub fn jwks_ec_key(issuer: &str, kid: Option<&str>) -> Result<(Vec<u8>, Vec<u8>), AuthError> {
    let jwks = fetch_jwks(issuer)?;
    let jwk = select_key(&jwks, kid)
        .ok_or_else(|| AuthError::InvalidToken("no matching jwk".into()))?;
    if jwk.kty != "EC" {
        return Err(AuthError::InvalidToken("jwk is not EC".into()));
    }
    let x = decode_b64(jwk.x.as_deref())?;
    let y = decode_b64(jwk.y.as_deref())?;
    Ok((x, y))
}

fn decode_b64(v: Option<&str>) -> Result<Vec<u8>, AuthError> {
    let s = v.ok_or_else(|| AuthError::InvalidToken("jwk missing field".into()))?;
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| AuthError::InvalidToken("jwk field not base64url".into()))
}

/// Authorization-code exchange. POSTs to the token endpoint; client creds are
/// read from kv ("oidc:client-id", "oidc:client-secret", "oidc:issuer").
pub fn exchange_code(code: &str, redirect_uri: &str) -> Result<TokenPair, AuthError> {
    let issuer = kv::get("oidc:issuer")?
        .ok_or_else(|| AuthError::BackendUnavailable("oidc:issuer not configured".into()))?;
    let client_id = kv::get("oidc:client-id")?
        .ok_or_else(|| AuthError::BackendUnavailable("oidc:client-id not configured".into()))?;
    let client_secret = kv::get("oidc:client-secret")?.unwrap_or_default();
    let doc = discovery(&issuer)?;

    let form = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&client_secret={}",
        urlencode(code),
        urlencode(redirect_uri),
        urlencode(&client_id),
        urlencode(&client_secret),
    );
    let body = http_post_form(&doc.token_endpoint, &form)?;

    #[derive(Deserialize)]
    struct TokenResp {
        access_token: String,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        expires_in: u64,
    }
    let resp: TokenResp = serde_json::from_slice(&body)
        .map_err(|e| AuthError::BackendUnavailable(format!("token resp: {e}")))?;
    Ok(TokenPair {
        access_token: resp.access_token,
        refresh_token: resp.refresh_token,
        expires_in: resp.expires_in,
        session_id: None,
    })
}

// ---- minimal wasi:http client ------------------------------------------

fn http_get(url: &str) -> Result<Vec<u8>, AuthError> {
    request(Method::Get, url, None)
}

fn http_post_form(url: &str, form: &str) -> Result<Vec<u8>, AuthError> {
    request(Method::Post, url, Some(form.as_bytes()))
}

fn parse_url(url: &str) -> Result<(Scheme, String, String), AuthError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(AuthError::Malformed(format!("bad url scheme: {url}")));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].to_string()),
        None => (rest.to_string(), "/".to_string()),
    };
    Ok((scheme, authority, path))
}

fn request(method: Method, url: &str, body: Option<&[u8]>) -> Result<Vec<u8>, AuthError> {
    let (scheme, authority, path) = parse_url(url)?;
    let headers = Fields::new();
    if body.is_some() {
        let _ = headers.set(
            &"content-type".to_string(),
            &[b"application/x-www-form-urlencoded".to_vec()],
        );
    }
    let req = OutgoingRequest::new(headers);
    req.set_method(&method).map_err(|_| net_err("set method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net_err("set scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| net_err("set authority"))?;
    req.set_path_with_query(Some(&path)).map_err(|_| net_err("set path"))?;

    // Write request body if present.
    if let Some(bytes) = body {
        let out = req.body().map_err(|_| net_err("body"))?;
        {
            let stream = out.write().map_err(|_| net_err("write stream"))?;
            stream
                .blocking_write_and_flush(bytes)
                .map_err(|e| net_err(&format!("body write: {e:?}")))?;
        }
        crate::bindings::wasi::http::types::OutgoingBody::finish(out, None)
            .map_err(|_| net_err("finish body"))?;
    }

    let opts = RequestOptions::new();
    let future = outgoing_handler::handle(req, Some(opts))
        .map_err(|e| AuthError::BackendUnavailable(format!("http handle: {e:?}")))?;

    future.subscribe().block();
    let resp = future
        .get()
        .ok_or_else(|| net_err("no response"))?
        .map_err(|_| net_err("response taken"))?
        .map_err(|e| AuthError::BackendUnavailable(format!("http: {e:?}")))?;

    let status = resp.status();
    let incoming = resp.consume().map_err(|_| net_err("consume"))?;
    let stream = incoming.stream().map_err(|_| net_err("incoming stream"))?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => buf.extend_from_slice(&chunk),
            Err(StreamError::Closed) => break,
            Err(e) => return Err(net_err(&format!("read: {e:?}"))),
        }
    }

    if !(200..300).contains(&status) {
        return Err(AuthError::BackendUnavailable(format!("http status {status}")));
    }
    Ok(buf)
}

fn net_err(ctx: &str) -> AuthError {
    AuthError::BackendUnavailable(format!("http: {ctx}"))
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
