//! Photos: plan an upload, complete it, list and read them back, and take the
//! evaluator's signed result. `CONTRACT.md` is the shape of all of it.
//!
//! The lifecycle, as the `state` field records it:
//!
//!   uploading ──complete──▶ uploaded ──submit──▶ processing ──callback──▶ evaluated
//!                                                           └──callback──▶ failed
//!
//! `uploaded` exists so a failed submit is recoverable. Completing a multipart
//! upload twice is an error at the store (the upload id is gone after the first),
//! but submitting twice is not — `job_id` dedupes in `MEDIA_JOBS` — so a retry of
//! `complete` from `uploaded` or `processing` skips straight to the submit.
//!
//! Ownership is `owner == principal.subject`, or an admin for reads. No
//! `policy:guard` here: one attribute, one comparison, and the rule is the same
//! for every route, so a policy engine would be a second place to read it.

use crate::bindings::media::pipeline::jobs::{self as media, MediaError, PartEtag};
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::config::store as config;
use crate::bindings::wasi::http::types::Method;
use crate::bindings::webhook::sign::signer::{self, Scheme};
use crate::{audit, introspect, is_admin, now_secs, Reply, Route};
use serde_json::{json, Map, Value};

const PHOTOS: &str = "photos";
/// How long a signed rendition URL works. An hour outlives any page view and
/// is short enough that a URL pasted somewhere stops working the same day.
const SIGN_TTL_SECS: u32 = 3600;
/// Every field the callback carries that is worth keeping. `timings_ms` is
/// kept too: it is how an operator tells a slow photo from a slow machine.
const RESULT_FIELDS: &[&str] =
    &["backend", "sha256", "metadata", "renditions", "sharpness", "vision", "colour", "timings_ms"];

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "photos"]) => create(route, body),
        (Method::Get, ["api", "photos"]) => list(route),
        (Method::Get, ["api", "photos", id]) => get(route, id),
        (Method::Post, ["api", "photos", id, "complete"]) => complete(route, id, body),
        _ => Reply::err(404, "not_found"),
    }
}

/// A photo id as `comp-media` will accept it — it becomes part of an object key
/// (CONTRACT.md). Record ids are ULIDs and always pass; this keeps anything
/// else a caller typed into a path from reaching the store at all.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// One stored photo as a JSON object, its record id merged in.
fn doc(entry: &records::Entry) -> Map<String, Value> {
    let mut m = match serde_json::from_str::<Value>(&entry.data) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    m.insert("id".into(), json!(entry.id));
    m
}

fn str_of<'a>(m: &'a Map<String, Value>, key: &str) -> &'a str {
    m.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// Write `m` back over `entry`, guarded by the revision it was read at.
fn save(
    entry: &records::Entry,
    m: &Map<String, Value>,
) -> Result<records::Entry, records::StoreError> {
    let mut data = m.clone();
    // The id is the record's own, not part of its data.
    data.remove("id");
    records::update(PHOTOS, &entry.id, &Value::Object(data).to_string(), entry.revision)
}

/// `media:pipeline`'s refusal as an HTTP answer. The detail is passed through:
/// "over 512 MiB" or "extension .png not allowed" is what the person uploading
/// needs to read, and it names nothing about the deployment.
fn media_reply(e: MediaError) -> Reply {
    let (status, code, detail) = match e {
        MediaError::Refused(d) => (422, "media_refused", d),
        // 409 rather than 404: the PHOTO exists; what is missing is the store's
        // side of it (an upload id that expired or was already completed).
        MediaError::NotFound(d) => (409, "media_not_found", d),
        MediaError::Unavailable(d) => (503, "media_unavailable", d),
    };
    Reply::json(status, json!({"error": code, "detail": detail}))
}

fn config_value(key: &str) -> Option<String> {
    match config::get(key) {
        Ok(Some(v)) if !v.trim().is_empty() => Some(v.trim().to_string()),
        _ => None,
    }
}

#[derive(serde::Deserialize)]
struct CreateReq {
    #[serde(default)]
    filename: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    content_type: String,
}

/// `POST /api/photos` — make the record, then ask for the plan.
///
/// The record comes first because its id IS the photo id the plan's key is
/// built from. If the plan is refused the record is deleted again: a gallery
/// that fills with photos nobody could ever upload is a bug report per refusal.
fn create(route: &Route, body: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    if let Err(r) = crate::moderation::require_active(&principal) {
        return r;
    }
    let req = guestauth::guest_parse_body!(body, CreateReq);
    let filename = req.filename.trim().to_string();
    if filename.is_empty() {
        return Reply::err(400, "filename is required");
    }
    if req.size == 0 {
        return Reply::err(400, "size is required");
    }
    let data = json!({
        "owner": principal.subject,
        "filename": filename,
        "size": req.size,
        "content_type": req.content_type,
        "state": "uploading",
        "created_at": now_secs(),
    });
    let entry = match records::create(PHOTOS, &data.to_string(), &["owner".to_string()]) {
        Ok(e) => e,
        Err(_) => return Reply::err(500, "store_error"),
    };
    let plan = match media::start_upload(&entry.id, &filename, req.size, &req.content_type) {
        Ok(p) => p,
        Err(e) => {
            let _ = records::delete(PHOTOS, &entry.id);
            return media_reply(e);
        }
    };
    let mut m = doc(&entry);
    m.insert("upload_id".into(), json!(plan.upload_id));
    m.insert("key".into(), json!(plan.key));
    let entry = match save(&entry, &m) {
        Ok(e) => e,
        Err(_) => {
            // The plan exists at the store and nothing here remembers it.
            let _ = media::abort_upload(&plan.upload_id, &plan.key);
            let _ = records::delete(PHOTOS, &entry.id);
            return Reply::err(500, "store_error");
        }
    };
    audit("photo.create", "allow", &principal.subject, &entry.id);
    let parts: Vec<Value> =
        plan.parts.iter().map(|p| json!({"number": p.number, "url": p.url})).collect();
    Reply::json(
        201,
        json!({
            "photo": Value::Object(doc(&entry)),
            "upload": {
                "upload_id": plan.upload_id,
                "key": plan.key,
                "part_size": plan.part_size,
                "parts": parts,
                "expires_at": plan.expires_at,
            },
        }),
    )
}

#[derive(serde::Deserialize)]
struct CompleteReq {
    #[serde(default)]
    parts: Vec<PartReq>,
}

#[derive(serde::Deserialize)]
struct PartReq {
    number: u32,
    etag: String,
}

/// `POST /api/photos/{id}/complete` — stitch the parts, then queue evaluation.
fn complete(route: &Route, id: &str, body: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    if let Err(r) = crate::moderation::require_active(&principal) {
        return r;
    }
    if !valid_id(id) {
        return Reply::err(404, "not_found");
    }
    let req = guestauth::guest_parse_body!(body, CompleteReq);
    let entry = guestauth::guest_get_or_404!(PHOTOS, id);
    let mut m = doc(&entry);
    // Owner only — not even an admin completes somebody else's upload.
    guestauth::guest_deny_unless!(
        str_of(&m, "owner") == principal.subject,
        principal,
        "photo.complete",
        id
    );
    // Checked before anything irreversible: completing the upload and then
    // finding there is nowhere to send the result would leave an original
    // nothing will ever evaluate.
    let Some(base) = config_value("public-callback-base") else {
        return Reply::err(503, "public_callback_base_unset");
    };
    let state = str_of(&m, "state").to_string();
    let key = str_of(&m, "key").to_string();
    let mut entry = entry;
    match state.as_str() {
        "uploading" => {
            if req.parts.is_empty() {
                return Reply::err(400, "parts is required");
            }
            let parts: Vec<PartEtag> = req
                .parts
                .iter()
                .map(|p| PartEtag { number: p.number, etag: p.etag.clone() })
                .collect();
            if let Err(e) = media::complete_upload(str_of(&m, "upload_id"), &key, &parts) {
                return media_reply(e);
            }
            m.insert("state".into(), json!("uploaded"));
            m.insert("uploaded_at".into(), json!(now_secs()));
            entry = match save(&entry, &m) {
                Ok(e) => e,
                Err(_) => return Reply::err(500, "store_error"),
            };
        }
        // A retry after a submit that failed, or a double-click: the original
        // is already whole, and the submit below is deduplicated by job id.
        "uploaded" | "processing" => {}
        _ => return Reply::json(409, json!({"error": "already_evaluated", "state": state})),
    }

    // The job id IS the photo id, so a second submit for the same photo is the
    // same message to `MEDIA_JOBS` and is dropped there (CONTRACT.md).
    let callback_url = format!("{}/internal/photos/{id}/evaluated", base.trim_end_matches('/'));
    if let Err(e) = media::submit(id, id, &key, &callback_url) {
        return media_reply(e);
    }
    if str_of(&m, "state") != "processing" {
        m.insert("state".into(), json!("processing"));
        m.insert("job_id".into(), json!(id));
        m.insert("submitted_at".into(), json!(now_secs()));
        if save(&entry, &m).is_err() {
            // The job is queued either way; a record that still says
            // `uploaded` is corrected by the callback, which accepts both.
            return Reply::err(500, "store_error");
        }
    }
    audit("photo.complete", "allow", &principal.subject, id);
    Reply::json(200, json!({"id": id, "state": "processing"}))
}

/// A signed GET for one rendition's key, or null. A failed sign never fails the
/// read around it: the record is still worth showing without its picture.
fn signed(m: &Map<String, Value>, rendition: &str) -> Value {
    let key = m
        .get("renditions")
        .and_then(|r| r.get(rendition))
        .and_then(|r| r.get("key"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if key.is_empty() {
        return Value::Null;
    }
    media::sign_get(key, SIGN_TTL_SECS).map(Value::String).unwrap_or(Value::Null)
}

/// `GET /api/photos` — the caller's own photos, newest first.
///
/// The gallery view: everything but the heavy parts of the result (256
/// sharpness tiles per photo add up), plus a signed thumbnail when there is one.
fn list(route: &Route) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let owner = serde_json::to_string(&principal.subject).unwrap_or_default();
    // `find-by` answers every match through the owner index — no page to follow.
    let entries = match records::find_by(PHOTOS, "owner", &owner) {
        Ok(e) => e,
        Err(_) => return Reply::err(500, "store_error"),
    };
    let mut photos: Vec<Map<String, Value>> = entries.iter().map(doc).collect();
    // Record ids are ULIDs, so id order is creation order even within a second.
    photos.sort_by(|a, b| {
        let at = |m: &Map<String, Value>| m.get("created_at").and_then(Value::as_u64).unwrap_or(0);
        at(b).cmp(&at(a)).then_with(|| str_of(b, "id").cmp(str_of(a, "id")))
    });
    let out: Vec<Value> = photos
        .into_iter()
        .map(|mut m| {
            let thumb =
                if str_of(&m, "state") == "evaluated" { signed(&m, "thumb") } else { Value::Null };
            if let Some(Value::Object(s)) = m.get_mut("sharpness") {
                s.remove("tiles");
            }
            m.insert("thumb_url".into(), thumb);
            owner_view_of_moderation(&mut m);
            Value::Object(m)
        })
        .collect();
    Reply::json(200, json!({"photos": out}))
}

/// `GET /api/photos/{id}` — the whole record, with signed rendition URLs.
fn get(route: &Route, id: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    if !valid_id(id) {
        return Reply::err(404, "not_found");
    }
    // 404 first: a caller must learn "not found" for a genuinely missing id,
    // rather than be told "forbidden" about something that never existed.
    let entry = guestauth::guest_get_or_404!(PHOTOS, id);
    let mut m = doc(&entry);
    guestauth::guest_deny_unless!(
        str_of(&m, "owner") == principal.subject || is_admin(&principal),
        principal,
        "photo.read",
        id
    );
    // A hidden photo is signed for its owner only — an admin reviewing it sees
    // the record, not a working link to pass around (CONTRACT.md "Hidden photo").
    let hidden_from_caller =
        crate::moderation::is_hidden(&m) && str_of(&m, "owner") != principal.subject;
    if !is_admin(&principal) {
        owner_view_of_moderation(&mut m);
    }
    let urls = if str_of(&m, "state") == "evaluated" && !hidden_from_caller {
        json!({"thumb": signed(&m, "thumb"), "share": signed(&m, "share"), "ai": signed(&m, "ai")})
    } else {
        json!({"thumb": null, "share": null, "ai": null})
    };
    m.insert("urls".into(), urls);
    Reply::json(200, Value::Object(m))
}

/// The owner sees `moderation: {hidden, reason, at}` — not which admin did it.
fn owner_view_of_moderation(m: &mut Map<String, Value>) {
    if let Some(Value::Object(mo)) = m.get_mut("moderation") {
        mo.remove("by");
    }
}

/// `POST /internal/photos/{id}/evaluated` — `comp-media`'s signed result.
///
/// Not behind a login, so the order matters: the signature is checked before
/// the photo is even looked up, and an unsigned caller learns nothing — not
/// even whether the id exists.
///
/// Idempotent, because the sender retries until it sees a 2xx: a result for a
/// photo that is already `evaluated` is acknowledged and changes nothing, and
/// `quests::on_evaluated` runs only on the transition INTO `evaluated`, so a
/// retried callback can never score the same photo twice.
pub fn evaluated(id: &str, raw: &[u8], signature: &str) -> Reply {
    // No secret configured means no callback can be trusted. Refusing is right,
    // and 503 rather than 401 tells the sender to keep retrying: this is a
    // deployment waiting to be finished, not a forgery.
    let Some(secret) = config_value("media-callback-secret") else {
        return Reply::err(503, "callback_secret_unset");
    };
    if signature.is_empty() || signer::verify(raw, signature, &secret, Scheme::Github, 0).is_err() {
        audit("photo.callback", "deny", "comp-media", id);
        return Reply::err(401, "bad_signature");
    }
    let result: Value = match serde_json::from_slice(raw) {
        Ok(v @ Value::Object(_)) => v,
        _ => return Reply::err(400, "bad_json"),
    };
    if result.get("photo_id").and_then(Value::as_str) != Some(id) {
        return Reply::err(400, "photo_id does not match the path");
    }
    let status = result.get("status").and_then(Value::as_str).unwrap_or_default();
    if status != "done" && status != "failed" {
        return Reply::err(400, "status must be done or failed");
    }
    if !valid_id(id) {
        return Reply::err(404, "not_found");
    }

    // A revision conflict here is the `complete` route writing `processing`
    // at the same moment — rare, and a re-read settles it.
    for _ in 0..3 {
        let entry = guestauth::guest_get_or_404!(PHOTOS, id);
        let mut m = doc(&entry);
        let job = result.get("job_id").and_then(Value::as_str).unwrap_or_default();
        let expected_job = match str_of(&m, "job_id") {
            "" => id,
            j => j,
        };
        if job != expected_job {
            return Reply::json(409, json!({"error": "job_mismatch", "expected": expected_job}));
        }
        let state = str_of(&m, "state").to_string();
        match (state.as_str(), status) {
            // A result for a photo that was never submitted.
            ("uploading", _) => {
                return Reply::json(409, json!({"error": "not_submitted", "state": state}))
            }
            // Already settled. A late `failed` after a `done` does not undo it.
            ("evaluated", _) | ("failed", "failed") => {
                return Reply::json(200, json!({"id": id, "state": state, "duplicate": true}));
            }
            _ => {}
        }

        for f in RESULT_FIELDS {
            if let Some(v) = result.get(*f) {
                m.insert((*f).to_string(), v.clone());
            }
        }
        let new_state = if status == "done" { "evaluated" } else { "failed" };
        m.insert("state".into(), json!(new_state));
        m.insert("evaluated_at".into(), json!(now_secs()));
        m.insert("job_id".into(), json!(job));
        if status == "done" {
            m.insert("error".into(), Value::Null);
        } else {
            let error = result.get("error").and_then(Value::as_str).unwrap_or("evaluation failed");
            m.insert("error".into(), json!(error));
        }

        match save(&entry, &m) {
            Ok(_) => {
                if new_state == "evaluated" {
                    crate::quests::on_evaluated(&m, &result);
                }
                audit("photo.evaluated", "allow", "comp-media", id);
                return Reply::json(200, json!({"id": id, "state": new_state}));
            }
            Err(records::StoreError::RevisionConflict(_)) => continue,
            Err(_) => return Reply::err(500, "store_error"),
        }
    }
    // Not 2xx, so the sender retries — which is exactly right.
    Reply::err(503, "busy")
}

#[cfg(test)]
mod tests {
    use super::valid_id;

    #[test]
    fn a_record_id_is_a_valid_photo_id() {
        assert!(valid_id("01K5Y2Z7Q9ABCDEF0123456789"));
        assert!(valid_id("a_b-c"));
    }

    #[test]
    fn anything_that_could_steer_an_object_key_is_not() {
        assert!(!valid_id(""));
        assert!(!valid_id("../originals/x"));
        assert!(!valid_id("a/b"));
        assert!(!valid_id("a.b"));
        assert!(!valid_id(&"x".repeat(65)));
    }
}
