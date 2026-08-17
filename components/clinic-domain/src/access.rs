//! Staff accounts and pet search — all three capabilities are called, none are
//! reimplemented.
//!
//! Index population: the store is the source of truth and `owners-and-pets` is
//! not ours to edit, so the index is *reconciled* rather than rebuilt on every
//! search — `doc-count` is asked first and the corpus is only re-fed when the
//! counts disagree. That keeps the common search path at one `query` call
//! instead of N `index-doc` calls, and needs no cooperation from the other half.

use crate::bindings::auth::identity::accounts;
use crate::bindings::auth::identity::session;
use crate::bindings::auth::identity::types::AuthError;
use crate::bindings::records::store::store as records;
use crate::bindings::search::index::index;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use serde_json::{json, Value};

const PETS: &str = "pets";
const TENANT: &str = "";
const PET_TAG: &str = "kind:pet";

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "staff"]) => register(body),
        (Method::Post, ["api", "staff", "login"]) => login(body),
        (Method::Get, ["api", "pets", "search"]) => search(route),
        _ => Reply::err(404, "not_found"),
    }
}

fn credentials(body: &str) -> Result<(String, String), Reply> {
    let doc: Value = serde_json::from_str(body).map_err(|_| Reply::err(400, "invalid"))?;
    let email = doc["email"].as_str().unwrap_or("").trim().to_string();
    let password = doc["password"].as_str().unwrap_or("").to_string();
    Ok((email, password))
}

fn register(body: &str) -> Reply {
    let (email, password) = match credentials(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    // The contract fixes the floor at 8; `password-min-len` is deployment config
    // and could be set lower, so this one is asserted here as well.
    if email.is_empty() || password.len() < 8 {
        return Reply::err(400, "invalid");
    }
    match accounts::register(&email, &password, TENANT) {
        Ok(p) => Reply::json(201, json!({ "id": p.subject, "email": email })),
        Err(AuthError::AlreadyExists) => Reply::err(409, "taken"),
        Err(AuthError::BackendUnavailable(_)) => Reply::err(503, "unavailable"),
        Err(_) => Reply::err(400, "invalid"),
    }
}

fn login(body: &str) -> Reply {
    let (email, password) = match credentials(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    match accounts::login(&email, &password, TENANT) {
        Ok(pair) => Reply::json(200, json!({ "token": pair.access_token })),
        // Unknown email and wrong password answer alike, on purpose.
        Err(_) => Reply::err(401, "unauthorized"),
    }
}

/// Bring the index level with the store, cheaply.
///
/// ponytail: `doc-count` vs the pet count is the whole staleness test — a
/// delete-plus-create between two searches leaves the counts equal and the
/// index one document behind. Swap for a per-pet revision check if pets ever
/// become editable.
fn reconcile(entries: &[records::Entry]) {
    if index::doc_count().unwrap_or(0) as usize == entries.len() {
        return;
    }
    for e in entries {
        let Ok(doc) = serde_json::from_str::<Value>(&e.data) else { continue };
        let text = format!(
            "{} {}",
            doc["name"].as_str().unwrap_or(""),
            doc["species"].as_str().unwrap_or("")
        );
        let _ = index::index_doc(&e.id, &text, &[PET_TAG.to_string()]);
    }
}

fn search(route: &Route) -> Reply {
    if route.bearer.is_empty() || session::lookup(&route.bearer).is_err() {
        return Reply::err(401, "unauthorized");
    }
    let q = route.param("q");
    if q.trim().is_empty() {
        return Reply::err(400, "invalid");
    }
    let page = match records::list_records(PETS, 1000, "") {
        Ok(p) => p,
        Err(_) => return Reply::err(503, "unavailable"),
    };
    reconcile(&page.entries);

    let hits = match index::query(&q, index::Mode::Any, &[PET_TAG.to_string()], 50) {
        Ok(h) => h,
        Err(_) => return Reply::err(503, "unavailable"),
    };
    // Ranked order is the index's answer; this only re-attaches the documents.
    let pets: Vec<Value> = hits
        .iter()
        .filter_map(|hit| {
            let entry = page.entries.iter().find(|e| e.id == hit.id)?;
            let doc: Value = serde_json::from_str(&entry.data).ok()?;
            Some(json!({
                "id": hit.id,
                "name": doc["name"],
                "species": doc["species"],
                "owner_id": doc["owner_id"],
            }))
        })
        .collect();
    Reply::json(200, json!({ "pets": pets }))
}