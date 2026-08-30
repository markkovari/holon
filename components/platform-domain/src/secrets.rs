//! The vault's HTTP surface: what a manifest may reference, and how a host reads it.
//!
//! Split out of `lib.rs` because it is the one part of this component where the
//! mistakes are not about shape but about DISCLOSURE. Everything here exists to
//! keep a plaintext from leaving except through one door, and putting that door
//! next to the deployment routes made it harder to see that it is the only one.
//!
//! Three rules the rest of the file does not have to think about:
//!
//!   * a secret is named per ORGANISATION (`vault_name`), because one vault backs
//!     the whole platform and a global namespace is ADR-0012's bucket mistake in a
//!     place where the consequence is worse;
//!   * `vault://<org>/<name>` is the only form a manifest may carry (ADR-0010) —
//!     never a value;
//!   * the plaintext leaves in `secret_fetch` and nowhere else, guarded by a token
//!     that expires and authorises one reference.

use serde_json::{json, Map, Value};

use crate::bindings::wasi::http::types::IncomingRequest;
use crate::req;
use crate::bindings::secrets::vault::vault;
use crate::{
    body, caller, claim_fetch_nonce, internal_ok, now, orgs, personal_org, read_body, records,
    str_of, Outcome,
};
/// Secrets are named per ORGANISATION, never globally.
///
/// One vault backs the whole platform, so the org has to be part of the name or two
/// tenants would share a namespace — the same mistake ADR-0012 measured with storage
/// buckets, in a place where the consequence is worse.
pub fn vault_name(org: &str, name: &str) -> String {
    format!("{org}/{name}")
}

/// `vault://<org>/<name>` — the only form a manifest may contain (ADR-0010).
pub fn parse_ref(r: &str) -> Option<(String, String)> {
    let rest = r.strip_prefix("vault://")?;
    let (org, name) = rest.split_once('/')?;
    if org.is_empty() || name.is_empty() {
        return None;
    }
    Some((org.to_string(), name.to_string()))
}

/// Store a secret for an org. The value is written straight through to the vault,
/// which seals it before it touches storage — nothing here keeps it, logs it, or puts
/// it in a response.
/// Where fetch tokens live. One row per instance that was granted a secret.
pub(crate) const FETCH_TOKENS: &str = "fetch_tokens";

/// Mint a capability for one instance: exactly these references, for a bounded time.
///
/// Issued BY the platform rather than signed by the reconciler, which is the simpler
/// and stronger arrangement — no shared signing key, and revocation is deleting a
/// row rather than waiting out a signature. The reconciler authenticates with the
/// platform secret it already holds (ADR-0003).
///
/// The token is a capability, not a secret value: it is worth exactly what this
/// manifest was worth, which is why the host may keep it in a ledger on disk
/// (ADR-0022).
pub fn fetch_token_mint(request: &IncomingRequest) -> Outcome {
    if !internal_ok(request) {
        return Outcome::Err(401, "bad platform secret".into());
    }
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let instance = str_of(&b, "instance");
    let refs: Vec<String> = b["refs"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    // `refs` may be empty. This started as a SECRETS credential, minted only for
    // instances that had any — but it is really an instance's proof of who it is,
    // and ADR-0079 needs that for an instance with no secrets at all. An empty
    // ref list simply authorises no secret.
    if instance.is_empty() {
        return Outcome::Err(422, "instance is required".into());
    }
    // Long enough to outlive an instance's useful life, short enough that a leaked
    // token is not a standing grant. A restart mints a new one, and a start costs
    // 0.43ms (ADR-0040), so a short life is cheap here in a way it usually is not.
    let ttl = b["ttl"].as_u64().unwrap_or(3600);
    // The record id is the token: unguessable, unique, and already stored — the same
    // trick the invite codes use (ADR-0031).
    let doc = json!({
        "instance": instance, "refs": refs, "expires": now() + ttl, "issued": now(),
    });
    match records::create(FETCH_TOKENS, &doc.to_string(), &["instance".to_string()]) {
        Ok(rec) => {
            Outcome::Json(201, json!({ "token": rec.id, "expires": now() + ttl }).to_string())
        }
        Err(_) => Outcome::Err(500, "could not mint a fetch token".into()),
    }
}

/// Resolve one reference for a host holding a valid token.
///
/// The plaintext leaves the platform here and nowhere else. Three checks, in this
/// order, because each is cheaper than the next: does the token exist, has it
/// expired, and does it authorise THIS reference.
pub fn secret_fetch(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let token = request
        .headers()
        .get("x-fetch-token")
        .into_iter()
        .next()
        .and_then(|v| String::from_utf8(v).ok())
        .unwrap_or_default();
    if token.is_empty() {
        return Outcome::Err(401, "no fetch token".into());
    }
    // Replay protection (ADR-0071). Without it a captured fetch could be replayed
    // against the platform for the rest of the token's life — the gap ADR-0051
    // named and did not close.
    if let Err(o) = claim_fetch_nonce(request) {
        return o;
    }
    let reference = query.get("ref").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let Ok(entry) = records::get(FETCH_TOKENS, &token) else {
        return Outcome::Err(401, "unknown fetch token".into());
    };
    let Ok(doc) = serde_json::from_str::<Value>(&entry.data) else {
        return Outcome::Err(401, "unreadable fetch token".into());
    };
    if doc["expires"].as_u64().unwrap_or(0) < now() {
        // 401 so the host can tell "restart me" from "your manifest is wrong".
        let _ = records::delete(FETCH_TOKENS, &token);
        return Outcome::Err(401, "fetch token expired".into());
    }
    let granted = doc["refs"]
        .as_array()
        .map(|a| a.iter().any(|r| r.as_str() == Some(reference.as_str())))
        .unwrap_or(false);
    if !granted {
        // 403, not 404: this token is real and this reference is not on it. Saying
        // so does not leak whether the secret exists, only that this instance was
        // not granted it — which the instance's own manifest already told it.
        return Outcome::Err(403, "this instance was not granted that reference".into());
    }
    let Some((org, name)) = parse_ref(&reference) else {
        return Outcome::Err(422, "not a secret reference".into());
    };
    // `?probe=1` is a host asking "does this resolve", which it does at START for
    // every reference in a manifest (ADR-0051). Identical authorisation — the token
    // checks above are the same ones — answered from `describe`, so no plaintext is
    // read, logged, or put on the wire for a secret nothing has revealed yet.
    if query.get("probe").is_some() {
        return match vault::describe(&vault_name(&org, &name)) {
            Ok(_) => Outcome::Json(200, json!({ "resolves": true }).to_string()),
            Err(vault::VaultError::NotFound) => Outcome::Err(404, "no such secret".into()),
            Err(e) => Outcome::Err(500, vault_detail(&e)),
        };
    }
    match vault::get(&vault_name(&org, &name)) {
        // Bytes, not JSON: a plaintext should not pass through a serialiser that
        // might log or escape it.
        Ok(v) => Outcome::Bytes(200, "application/octet-stream".into(), v),
        Err(vault::VaultError::NotFound) => Outcome::Err(404, "no such secret".into()),
        Err(e) => Outcome::Err(500, vault_detail(&e)),
    }
}

pub fn secret_put(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    // Writing a secret is not a viewer's job.
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Member) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
    let b: req::PutSecret = match read_body(request)
        .map_err(|_| Outcome::Err(400, "could not read body".into()))
        .and_then(|raw| req::parse(&raw))
    {
        Ok(v) => v,
        Err(o) => return o,
    };
    let name = b.name.trim();
    if name.is_empty() || b.value.is_empty() {
        return Outcome::Err(422, "name and value are both required".into());
    }
    match vault::put(&vault_name(&org, name), b.value.as_bytes()) {
        // The reply is metadata, deliberately: a caller that just wrote a secret has
        // the value already, and echoing it back puts it in one more place.
        Ok(meta) => Outcome::Json(
            201,
            json!({
                "ref": format!("vault://{org}/{name}"),
                "name": name, "org": org,
                "version": meta.version, "updated": meta.updated,
            })
            .to_string(),
        ),
        Err(e) => Outcome::Err(500, format!("vault refused the write: {}", vault_detail(&e))),
    }
}

/// Names only. There is no endpoint that returns a value: the platform stores
/// secrets so that workloads can use them, not so that a browser can display them.
pub fn secrets_list(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Viewer) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
    let prefix = format!("{org}/");
    match vault::list_names(500) {
        Ok(names) => {
            let mine: Vec<Value> = names
                .iter()
                .filter_map(|n| n.strip_prefix(&prefix))
                .map(|n| json!({ "name": n, "ref": format!("vault://{org}/{n}") }))
                .collect();
            Outcome::Json(200, json!({ "secrets": mine, "count": mine.len() }).to_string())
        }
        Err(e) => Outcome::Err(500, format!("vault unreadable: {}", vault_detail(&e))),
    }
}

pub fn secret_delete(request: &IncomingRequest, name: &str, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Member) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
    match vault::delete(&vault_name(&org, name)) {
        Ok(()) => Outcome::Json(200, json!({ "deleted": name, "org": org }).to_string()),
        Err(e) => Outcome::Err(500, format!("vault refused the delete: {}", vault_detail(&e))),
    }
}

pub fn vault_detail(e: &vault::VaultError) -> String {
    match e {
        vault::VaultError::NotFound => "no such secret".into(),
        vault::VaultError::Crypto(m) => format!("crypto: {m}"),
        vault::VaultError::BackendUnavailable(m) => format!("backend unavailable: {m}"),
    }
}

/// Every secret a component asks for must resolve, and must belong to the org
/// deploying it.
///
/// `describe` is the whole reason this is safe: it answers "is there a secret by this
/// name" WITHOUT decrypting, so a save can be validated without the platform ever
/// holding a plaintext it has no use for (ADR-0010).
pub fn check_secrets(id: &str, org: &str, secrets: &[Value]) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    for s in secrets {
        let key = s["key"].as_str().unwrap_or_default().trim();
        let reference = s["ref"].as_str().unwrap_or_default().trim();
        if key.is_empty() || reference.is_empty() {
            return Err(format!("`{id}`: every secret needs a key and a ref"));
        }
        let Some((ref_org, name)) = parse_ref(reference) else {
            return Err(format!(
                "`{id}`: `{reference}` is not a secret reference — it must look like `vault://{org}/<name>`"
            ));
        };
        // Refusing another org's reference is the whole boundary. Without it a
        // manifest could name any secret on the platform and the vault would happily
        // resolve it.
        if ref_org != org {
            return Err(format!(
                "`{id}`: `{reference}` belongs to `{ref_org}`, and this deployment is for `{org}`"
            ));
        }
        if vault::describe(&vault_name(&ref_org, &name)).is_err() {
            return Err(format!(
                "`{id}`: `{reference}` does not resolve — store it first with POST /api/secrets"
            ));
        }
        // BY REFERENCE ONLY. The value is never read here, so it cannot reach a
        // manifest, a revision, or a log line.
        out.push(json!({ "key": key, "ref": reference }));
    }
    Ok(out)
}
