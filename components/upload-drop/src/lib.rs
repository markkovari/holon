//! drop:app — a presigned direct-upload drop-box over composed contracts.
//!
//! The presigned-ticket axis: the backend never streams the payload on the way
//! IN. `POST /api/tickets` answers only the policy question via `upload::gate`
//! (content-type allowed? under the size cap?) and returns a signed, expiring
//! ticket. The client then `PUT`s the bytes at `/api/blob/{token}`; we
//! `upload::redeem` the ticket (verifying the HMAC + expiry), store the bytes in
//! `blob::store`, and write the object's metadata to `records::store`.
//!
//! Downloads go out under a `webhook::sign` HMAC token: `/api/object/{id}`
//! returns a `{sig, exp}` pair, and `/api/blob/{id}?sig=&exp=` streams the bytes
//! only if the signature verifies and hasn't expired. A shareable link that
//! never exposes the underlying store — no auth component needed.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Value};

use bindings::blob::store::blobstore as blob;
use bindings::records::store::store as records;
use bindings::upload::policy::gate as upload;
use bindings::wasi::clocks::wall_clock;
use bindings::webhook::sign::signer as sign;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const META: &str = "objects";
const CONTAINER: &str = "drop";
/// The download-link secret. Not a request secret — a deploy-time signing key
/// for the shareable link HMAC. Swap via a real secret in production.
const LINK_SECRET: &str = "drop-download-link-secret";
/// Signed download links live this long.
const LINK_TTL: u64 = 300;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage_json(),
            (Method::Post, ["api", "tickets"]) => make_ticket(&request),
            (Method::Put, ["api", "blob", token]) => put_blob(&request, token),
            (Method::Get, ["api", "objects"]) => list_objects(),
            (Method::Get, ["api", "object", id]) => object_meta(id),
            (Method::Get, ["api", "blob", id]) => get_blob(id, &path),
            (Method::Get, ["api", "stats"]) => stats(),
            _ => Outcome::err(404, "not_found"),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
    /// Raw bytes with an explicit content-type (the download path).
    Bytes(u16, String, Vec<u8>),
}
impl Outcome {
    fn err(code: u16, msg: &str) -> Outcome {
        Outcome::Json(code, json!({ "error": msg }).to_string())
    }
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn usage_json() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "drop",
            "about": "presigned direct-upload drop-box — the backend authorizes + signs, it never proxies the upload",
            "ticket": "POST /api/tickets {content-type, size} -> {token, object-key, expires}",
            "upload": "PUT /api/blob/{token}  (raw body) -> {id}",
            "objects": "GET /api/objects",
            "object": "GET /api/object/{id} -> metadata + a signed download link",
            "download": "GET /api/blob/{id}?sig=&exp=",
            "stats": "GET /api/stats"
        })
        .to_string(),
    )
}

// ---- ticket: the policy answer ----------------------------------------------

/// Answer the policy question and mint a signed, expiring ticket. No bytes
/// touched here — this is the whole point of the presigned axis.
fn make_ticket(request: &IncomingRequest) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let content_type = body["content-type"].as_str().unwrap_or("").trim().to_string();
    let size = body["size"].as_u64().unwrap_or(0);
    if content_type.is_empty() {
        return Outcome::err(422, "content-type required");
    }
    // tenant is fixed for the demo; a real deploy keys it off the caller.
    match upload::authorize("drop", &content_type, size, 0) {
        Ok(t) => Outcome::Json(
            201,
            json!({"token": t.token, "object_key": t.object_key, "expires": t.expires}).to_string(),
        ),
        Err(e) => policy_err(e),
    }
}

// ---- upload: redeem + store --------------------------------------------------

/// Redeem the ticket (HMAC + expiry checked inside `upload::redeem`), store the
/// raw body under the granted object-key, and write metadata. The client PUTs
/// straight here — we stream the body into the store once, no intermediate copy
/// held for policy (policy was already decided at ticket time).
fn put_blob(request: &IncomingRequest, token: &str) -> Outcome {
    let grant = match upload::redeem(token) {
        Ok(g) => g,
        Err(e) => return policy_err(e),
    };
    let data = match read_body(request) {
        Ok(b) => b,
        Err(_) => return Outcome::err(400, "could not read body"),
    };
    // enforce the granted ceiling on the actual bytes (ticket carried max-size).
    if data.len() as u64 > grant.max_size {
        return Outcome::err(413, "body exceeds granted max-size");
    }
    // create the metadata record first so the store mints the id; the blob is
    // then keyed under that SAME id, so `GET /api/object/{id}` hydrates and the
    // download path reads the right object.
    let seed = json!({
        "object_key": grant.object_key,
        "content_type": grant.content_type,
        "size": data.len() as u64,
        "at": now(),
    });
    let entry = match records::create(META, &seed.to_string(), &[]) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    let id = entry.id.clone();
    if let Err(e) = blob::put(CONTAINER, &id, &data, &grant.content_type) {
        // bytes never landed — drop the orphaned metadata record.
        let _ = records::delete(META, &id);
        return blob_err(e);
    }
    // rewrite the record with its own id embedded for the listing/meta views.
    let doc = json!({
        "id": id,
        "object_key": grant.object_key,
        "content_type": grant.content_type,
        "size": data.len() as u64,
        "at": now(),
    });
    let _ = records::update(META, &id, &doc.to_string(), entry.revision);
    Outcome::Json(201, json!({"id": id, "size": data.len() as u64}).to_string())
}

// ---- metadata + signed link --------------------------------------------------

fn list_objects() -> Outcome {
    match records::list_records(META, 100, "") {
        Ok(page) => {
            let rows: Vec<Value> = page
                .entries
                .iter()
                .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
                .collect();
            Outcome::Json(200, json!({"objects": rows}).to_string())
        }
        Err(e) => store_err(e),
    }
}

/// Return the object's metadata plus a freshly signed, expiring download link.
fn object_meta(id: &str) -> Outcome {
    let entry = match records::get(META, id) {
        Ok(e) => e,
        Err(records::StoreError::NotFound) => return Outcome::err(404, "not_found"),
        Err(e) => return store_err(e),
    };
    let mut doc: Value = match serde_json::from_str(&entry.data) {
        Ok(v) => v,
        Err(_) => return Outcome::err(500, "corrupt metadata"),
    };
    let (sig, exp) = sign_link(id);
    doc["download"] = json!(format!("/api/blob/{id}?sig={sig}&exp={exp}"));
    Outcome::Json(200, doc.to_string())
}

/// Verify the signed link, then stream the stored bytes back out.
fn get_blob(id: &str, path: &str) -> Outcome {
    let sig = query_str(path, "sig").unwrap_or_default();
    let exp: u64 = query_str(path, "exp").and_then(|s| s.parse().ok()).unwrap_or(0);
    if !verify_link(id, exp, &sig) {
        return Outcome::err(403, "invalid or expired link");
    }
    let info = match blob::head(CONTAINER, id) {
        Ok(i) => i,
        Err(blob::BlobError::NotFound) => return Outcome::err(404, "not_found"),
        Err(e) => return blob_err(e),
    };
    match blob::get(CONTAINER, id) {
        Ok(bytes) => Outcome::Bytes(200, info.content_type, bytes),
        Err(e) => blob_err(e),
    }
}

/// Sign `id|exp` with the download secret using the signer's stripe scheme; the
/// signature header string is opaque to the client and re-verified on download.
fn sign_link(id: &str) -> (String, u64) {
    let exp = now() + LINK_TTL;
    let payload = format!("{id}|{exp}");
    match sign::sign(payload.as_bytes(), LINK_SECRET, sign::Scheme::Stripe) {
        Ok(s) => (s.header, exp),
        Err(_) => (String::new(), exp),
    }
}

fn verify_link(id: &str, exp: u64, sig: &str) -> bool {
    if exp < now() || sig.is_empty() {
        return false;
    }
    let payload = format!("{id}|{exp}");
    // tolerance is generous — we already gate on `exp` ourselves above.
    sign::verify(payload.as_bytes(), sig, LINK_SECRET, sign::Scheme::Stripe, LINK_TTL + 60).is_ok()
}

// ---- stats -------------------------------------------------------------------

fn stats() -> Outcome {
    let count = records::count(META).unwrap_or(0);
    let mut bytes: u64 = 0;
    if let Ok(page) = records::list_records(META, 1000, "") {
        for e in &page.entries {
            if let Ok(v) = serde_json::from_str::<Value>(&e.data) {
                bytes += v["size"].as_u64().unwrap_or(0);
            }
        }
    }
    Outcome::Json(200, json!({"objects": count, "total_bytes": bytes}).to_string())
}

// ---- error mapping -----------------------------------------------------------

fn policy_err(e: upload::PolicyError) -> Outcome {
    match e {
        upload::PolicyError::TypeNotAllowed(t) => {
            Outcome::Json(415, json!({"error": "type_not_allowed", "type": t}).to_string())
        }
        upload::PolicyError::TooLarge(max) => {
            Outcome::Json(413, json!({"error": "too_large", "max": max}).to_string())
        }
        upload::PolicyError::InvalidTicket => Outcome::err(403, "invalid_ticket"),
        upload::PolicyError::BackendUnavailable(m) => {
            Outcome::Json(503, json!({"error": m}).to_string())
        }
    }
}

fn blob_err(e: blob::BlobError) -> Outcome {
    match e {
        blob::BlobError::NotFound => Outcome::err(404, "not_found"),
        blob::BlobError::BackendUnavailable(m) => {
            Outcome::Json(503, json!({"error": m}).to_string())
        }
    }
}

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::err(404, "not_found"),
        records::StoreError::InvalidJson(m) => Outcome::Json(422, json!({"error": m}).to_string()),
        records::StoreError::RevisionConflict(_) => Outcome::err(409, "conflict"),
        records::StoreError::BackendUnavailable(m) => {
            Outcome::Json(503, json!({"error": m}).to_string())
        }
    }
}

// ---- http plumbing -----------------------------------------------------------

fn parse_body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let body = read_body(request).map_err(|_| Outcome::err(400, "could not read body"))?;
    if body.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_slice(&body)
        .map_err(|e| Outcome::Json(400, json!({"error": format!("bad json: {e}")}).to_string()))
}

/// A ceiling on a request body, not a policy: past this the read stops and the
/// caller is told, rather than growing until the store's memory cap traps the
/// component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_read_body!(MAX_BODY_BYTES);
guestio::guest_write_all!();

fn query_str(path: &str, key: &str) -> Option<String> {
    let query = path.split('?').nth(1)?;
    query.split('&').find_map(|pair| {
        let mut it = pair.splitn(2, '=');
        (it.next()? == key).then(|| decode(it.next().unwrap_or("")))
    })
}

use guestfmt::percent_decode as decode;

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => {
            respond(response_out, code, "application/json", body.as_bytes())
        }
        Outcome::Bytes(code, ct, bytes) => respond(response_out, code, &ct, &bytes),
    }
}

fn respond(response_out: ResponseOutparam, status: u16, content_type: &str, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[content_type.as_bytes().to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        let _ = write_all(&stream, body);
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
