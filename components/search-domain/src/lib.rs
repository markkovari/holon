//! search:app — faceted search-as-you-type over composed contracts.
//!
//! The query pipeline: tokenize the box → build a cache key → `cache::get`;
//! on a MISS run `search::query` (TF-IDF ranked ids) → hydrate each id from
//! `records::store` → page (offset/limit) → `cache::set` the JSON. Every query
//! bumps a `search:hit` or `search:miss` counter in `metrics::collect`, so the
//! console can show a live hit-ratio — the read-path headline. No SSE: search
//! is request/response; the new thing here is the query/read axis.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Value};

use bindings::cache::store::cache;
use bindings::metrics::collect::collector as metrics;
use bindings::records::store::store as records;
use bindings::search::index::index as search;
use bindings::wasi::clocks::wall_clock;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const DOCS: &str = "documents";
const CACHE_TTL: u64 = 60;
const HIT: &str = "search:hit";
const MISS: &str = "search:miss";

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage_json(),
            (Method::Get, ["api", "search"]) => do_search(&path),
            (Method::Get, ["api", "doc", id]) => get_doc(id),
            (Method::Post, ["api", "index"]) => index_doc(&request),
            (Method::Post, ["api", "seed"]) => seed(),
            (Method::Get, ["api", "stats"]) => stats(),
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn usage_json() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "search",
            "about": "faceted search-as-you-type — TF-IDF ranked, tag facets, cache hit-ratio; the read/query axis",
            "search": "GET /api/search?q=&mode=any|all&tags=a,b&limit=10&offset=0",
            "doc": "GET /api/doc/{id}",
            "index": "POST /api/index {id?, title, body, tags:[..]}",
            "seed": "POST /api/seed",
            "stats": "GET /api/stats"
        })
        .to_string(),
    )
}

// ---- query pipeline ----------------------------------------------------------

fn do_search(path: &str) -> Outcome {
    let q = query_str(path, "q").unwrap_or_default();
    let mode_s = query_str(path, "mode").unwrap_or_else(|| "any".into());
    let tags_s = query_str(path, "tags").unwrap_or_default();
    let limit = query_i64(path, "limit").unwrap_or(10).clamp(1, 50) as u32;
    let offset = query_i64(path, "offset").unwrap_or(0).max(0) as usize;
    let tags: Vec<String> = tags_s.split(',').filter(|s| !s.is_empty()).map(String::from).collect();

    if q.trim().is_empty() {
        return Outcome::Json(
            200,
            json!({"hits": [], "total": 0, "cached": false, "ms": 0}).to_string(),
        );
    }

    let started = now();
    // cache key covers everything that changes the result.
    let ckey = format!("q/{mode_s}/{}/{offset}/{limit}/{}", tags.join("+"), q.trim());

    if let Ok(Some(bytes)) = cache::get(&ckey) {
        let _ = metrics::incr(HIT, 1);
        if let Ok(mut v) = serde_json::from_slice::<Value>(&bytes) {
            v["cached"] = json!(true);
            v["ms"] = json!(now().saturating_sub(started));
            return Outcome::Json(200, v.to_string());
        }
    }
    let _ = metrics::incr(MISS, 1);

    let mode =
        if mode_s.eq_ignore_ascii_case("all") { search::Mode::All } else { search::Mode::Any };
    // over-fetch so we can offset within the ranked list.
    let want = (offset as u32).saturating_add(limit).saturating_add(1);
    let hits = match search::query(q.trim(), mode, &tags, want) {
        Ok(h) => h,
        Err(e) => return search_err(e),
    };
    let total = hits.len();
    let page: Vec<&search::Hit> = hits.iter().skip(offset).take(limit as usize).collect();

    // hydrate each hit id from the corpus.
    let mut rows = Vec::with_capacity(page.len());
    for h in &page {
        if let Ok(entry) = records::get(DOCS, &h.id) {
            if let Ok(mut doc) = serde_json::from_str::<Value>(&entry.data) {
                doc["score"] = json!((h.score * 1000.0).round() / 1000.0);
                rows.push(doc);
            }
        }
    }

    let has_more = total > offset + page.len();
    let body = json!({
        "hits": rows,
        "total": total,
        "offset": offset,
        "has_more": has_more,
        "cached": false,
        "ms": now().saturating_sub(started),
    });
    // cache the miss result (without the per-call timing/cached flags — they're
    // re-stamped on a hit).
    let _ = cache::set(&ckey, body.to_string().as_bytes(), CACHE_TTL);
    Outcome::Json(200, body.to_string())
}

fn get_doc(id: &str) -> Outcome {
    match records::get(DOCS, id) {
        Ok(entry) => Outcome::Json(200, entry.data),
        Err(records::StoreError::NotFound) => Outcome::Err(404, "not_found".into()),
        Err(e) => store_err(e),
    }
}

// ---- write path --------------------------------------------------------------

fn index_doc(request: &IncomingRequest) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let title = body["title"].as_str().unwrap_or("").trim().to_string();
    let bodytext = body["body"].as_str().unwrap_or("").trim().to_string();
    if title.is_empty() && bodytext.is_empty() {
        return Outcome::Err(422, "title or body required".into());
    }
    let given = body["id"].as_str().map(String::from);
    let tags: Vec<String> = body["tags"]
        .as_array()
        .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let url = body["url"].as_str().unwrap_or("").to_string();
    persist_and_index(given, &title, &bodytext, &url, &tags)
}

/// Upsert the record + (re)index its postings + invalidate the query cache.
/// The record-store owns the id (it mints a ULID on create); we index under
/// that SAME id so a hit hydrates. `given` = an existing record id to re-index.
fn persist_and_index(
    given: Option<String>,
    title: &str,
    body: &str,
    url: &str,
    tags: &[String],
) -> Outcome {
    let id = match &given {
        // re-index an existing document.
        Some(id) => match records::get(DOCS, id) {
            Ok(existing) => {
                let doc = json!({"id": id, "title": title, "body": body, "url": url, "tags": tags, "at": now()});
                if let Err(e) = records::update(DOCS, id, &doc.to_string(), existing.revision) {
                    return store_err(e);
                }
                id.clone()
            }
            Err(records::StoreError::NotFound) => {
                return Outcome::Err(404, "unknown document id".into())
            }
            Err(e) => return store_err(e),
        },
        // new document: create first so we index under the store-minted id.
        None => {
            // create with a placeholder, then rewrite with the real id embedded.
            let seed = json!({"title": title, "body": body, "url": url, "tags": tags, "at": now()});
            let entry = match records::create(DOCS, &seed.to_string(), &["id".to_string()]) {
                Ok(e) => e,
                Err(e) => return store_err(e),
            };
            let doc = json!({"id": entry.id, "title": title, "body": body, "url": url, "tags": tags, "at": now()});
            let _ = records::update(DOCS, &entry.id, &doc.to_string(), entry.revision);
            entry.id
        }
    };
    let text = format!("{title} {body}");
    if let Err(e) = search::index_doc(&id, &text, tags) {
        return search_err(e);
    }
    // any write changes result sets — drop the whole query cache namespace.
    let _ = cache::invalidate_prefix("q/");
    Outcome::Json(201, json!({"id": id, "indexed": true}).to_string())
}

// ---- seed corpus -------------------------------------------------------------

fn seed() -> Outcome {
    let mut n = 0;
    for (title, body, tags) in CORPUS {
        let taglist: Vec<String> = tags.iter().map(|s| s.to_string()).collect();
        if let Outcome::Json(..) = persist_and_index(None, title, body, "", &taglist) {
            n += 1;
        }
    }
    Outcome::Json(200, json!({"seeded": n}).to_string())
}

/// A small demo corpus with overlapping + rare terms so ranking + facets show.
const CORPUS: &[(&str, &str, &[&str])] = &[
    (
        "WebAssembly components",
        "The component model composes wasm modules with typed WIT interfaces.",
        &["kind:doc", "topic:wasm"],
    ),
    (
        "Distributed sagas",
        "A saga coordinates a distributed transaction with compensating actions on failure.",
        &["kind:doc", "topic:distributed"],
    ),
    (
        "Server-sent events",
        "SSE holds an HTTP connection open and streams data frames to the browser.",
        &["kind:doc", "topic:realtime"],
    ),
    (
        "Feature flags and rollouts",
        "Percentage rollouts bucket subjects on a stable hash so cohorts stay sticky.",
        &["kind:doc", "topic:flags"],
    ),
    (
        "Rate limiting",
        "A fixed-window limiter counts failures and locks a key out until the window elapses.",
        &["kind:note", "topic:traffic"],
    ),
    (
        "Inverted index",
        "Search maps tokens to postings and ranks documents by TF-IDF over the corpus.",
        &["kind:doc", "topic:search"],
    ),
    (
        "Idempotency keys",
        "Exactly-once request handling dedups on a client-supplied idempotency key.",
        &["kind:note", "topic:reliability"],
    ),
    (
        "Transactional outbox",
        "The outbox enqueues an event in the same store as the write for at-least-once delivery.",
        &["kind:doc", "topic:reliability"],
    ),
    (
        "Durable timers",
        "A scheduler persists timers so a one-shot or recurring job survives a restart.",
        &["kind:note", "topic:scheduling"],
    ),
    (
        "Envelope encryption",
        "A vault seals secrets under a master key using AEAD, so rotation re-wraps data keys.",
        &["kind:doc", "topic:security"],
    ),
];

// ---- stats -------------------------------------------------------------------

fn stats() -> Outcome {
    let docs = search::doc_count().unwrap_or(0);
    let hit = metrics::get(HIT).unwrap_or(0);
    let miss = metrics::get(MISS).unwrap_or(0);
    let total = hit + miss;
    let ratio = if total == 0 { 0.0 } else { hit as f64 / total as f64 };
    Outcome::Json(
        200,
        json!({"docs": docs, "cache_hits": hit, "cache_misses": miss, "hit_ratio": (ratio * 1000.0).round() / 1000.0}).to_string(),
    )
}

// ---- http plumbing -----------------------------------------------------------

fn search_err(e: search::SearchError) -> Outcome {
    match e {
        search::SearchError::NotFound => Outcome::Err(404, "not_found".into()),
        search::SearchError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::Err(404, "not_found".into()),
        records::StoreError::InvalidJson(m) => Outcome::Err(422, m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

fn parse_body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let body = read_body(request).map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if body.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_slice(&body).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
}

/// The most a request body may be, before the component stops reading it.
///
/// There was no ceiling anywhere: 148 of 150 components accumulated whatever
/// arrived until the guest hit wasmtime's 64 MiB per-store memory cap and TRAPPED,
/// which reaches the caller as a closed connection saying nothing about a size.
/// A component that answers JSON has no business reading sixteen megabytes, and
/// the ones that legitimately handle uploads police it themselves with a 413 and a
/// granted max-size — those are left alone.
///
/// Generous on purpose. This is a backstop against an unbounded read, not a
/// content policy; an API that needs a real limit should state its own and say 413.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_read_body!(MAX_BODY_BYTES);

fn query_str(path: &str, key: &str) -> Option<String> {
    let query = path.split('?').nth(1)?;
    query.split('&').find_map(|pair| {
        let mut it = pair.splitn(2, '=');
        (it.next()? == key).then(|| decode(it.next().unwrap_or("")))
    })
}

fn query_i64(path: &str, key: &str) -> Option<i64> {
    query_str(path, key)?.parse().ok()
}

use guestfmt::percent_decode as decode;

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => respond(response_out, code, body.as_bytes()),
        Outcome::Err(code, msg) => {
            respond(response_out, code, json!({ "error": msg }).to_string().as_bytes())
        }
    }
}

fn respond(response_out: ResponseOutparam, status: u16, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        for chunk in body.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
