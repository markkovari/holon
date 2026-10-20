//! Every time rule reads `now()`, so an e2e run can close a competition without waiting.
//! The offset exists only with `allow-test-routes`; otherwise `POST /test/clock` is a 404.

use crate::bindings::records::store::store as records;
use crate::bindings::wasi::config::store as config;
use crate::{now_secs, Reply};

/// At most one record; ids are store-minted, so "the" record is the first one.
const CLOCK: &str = "test_clock";

fn current() -> Option<records::Entry> {
    records::list_records(CLOCK, 1, "").ok()?.entries.into_iter().next()
}

pub fn test_routes_allowed() -> bool {
    matches!(config::get("allow-test-routes"), Ok(Some(v)) if v == "true")
}

fn offset() -> i64 {
    if !test_routes_allowed() {
        return 0;
    }
    current()
        .and_then(|e| serde_json::from_str::<serde_json::Value>(&e.data).ok())
        .and_then(|v| v["offset_secs"].as_i64())
        .unwrap_or(0)
}

pub fn now() -> u64 {
    (now_secs() as i64 + offset()).max(0) as u64
}

/// Sets (not adds to) the offset.
pub fn set(body: &str) -> Reply {
    if !test_routes_allowed() {
        return Reply::err(404, "not_found");
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Reply::err(400, "bad_json");
    };
    let Some(off) = v["offset_secs"].as_i64() else {
        return Reply::err(400, "offset_secs is required");
    };
    let data = serde_json::json!({ "offset_secs": off }).to_string();
    let saved = match current() {
        Some(e) => records::update(CLOCK, &e.id, &data, e.revision).map(|_| ()),
        None => records::create(CLOCK, &data, &[]).map(|_| ()),
    };
    match saved {
        Ok(()) => Reply::json(200, serde_json::json!({ "offset_secs": off, "now": now() })),
        Err(e) => Reply::err(503, &format!("store: {e:?}")),
    }
}
