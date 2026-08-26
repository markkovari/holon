//! scribe:app — a collaborative document editor over composed contracts.
//!
//! A document is one `crdt:merge` `lwwmap` state string persisted in
//! `record:store` (one row per doc, looked up by an indexed `doc` field). An
//! edit is an op `{field, value, ts, replica}`: the server merges it with
//! `lwwmap-set` and stores the new state under optimistic concurrency (the
//! record `revision` is the CAS token) — on a conflict it reloads and re-merges,
//! which is safe precisely because CRDT merge is commutative + idempotent, so a
//! retried op still converges. `GET /events` holds the connection open and
//! pushes the merged document whenever its revision changes (real SSE push on
//! wasip2, same trick as pulse).

#[allow(warnings)]
mod bindings;

use serde_json::{json, Value};

use bindings::crdt::merge::merger as crdt;
use bindings::diff::text::differ as textdiff;
use bindings::id::generate::generator as ids;
use bindings::records::store::store as records;
use bindings::wasi::clocks::monotonic_clock;
use bindings::wasi::clocks::wall_clock;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

guestio::guest_write_all!();

struct Component;

const DOCS: &str = "docs";
const HISTORY: &str = "history";
const PRESENCE: &str = "presence";
const POLL_MS: u64 = 600;
const MAX_TICKS: u32 = 900; // ~9 min connection cap; the client's EventSource reconnects.
const PRESENCE_WINDOW: u64 = 15; // seconds since last heartbeat to count as "editing"
const MAX_RETRY: u32 = 8; // optimistic-merge retries on revision conflict
const MAX_FIELD_LEN: usize = 20_000;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        match (&method, seg.as_slice()) {
            // The SSE route owns the response (it streams); everything else
            // computes an Outcome and emits once.
            (Method::Get, ["api", "docs", doc, "events"]) => {
                stream_events(response_out, doc, &path);
            }
            _ => {
                let outcome = match (&method, seg.as_slice()) {
                    (Method::Get, [""]) => usage_json(),
                    (Method::Get, ["api", "docs", doc]) => get_doc(doc),
                    (Method::Get, ["api", "docs", doc, "history"]) => get_history(doc, &path),
                    (Method::Post, ["api", "docs", doc, "ops"]) => apply_op(&request, doc),
                    (Method::Post, ["api", "docs", doc, "presence"]) => heartbeat(&request, doc),
                    (Method::Get, ["api", "docs", doc, "presence"]) => presence(doc),
                    _ => Outcome::Err(404, "not_found".into()),
                };
                emit(response_out, outcome);
            }
        }
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
            "service": "scribe",
            "about": "collaborative editor — scalar fields are LWW registers, the body is an RGA text sequence; edits merge and stream live over SSE",
            "doc": "GET /api/docs/{doc}",
            "edit_field": "POST /api/docs/{doc}/ops {field, value, ts, replica}",
            "edit_body": "POST /api/docs/{doc}/ops {field:'body', kind:'insert'|'delete', after|ids, text, ts, replica, seq}",
            "stream": "GET /api/docs/{doc}/events?rev=n   (text/event-stream)",
            "presence": "POST|GET /api/docs/{doc}/presence"
        })
        .to_string(),
    )
}

// ---- document state ----------------------------------------------------------

/// The stored row for `doc`, if any: (record id, crdt lwwmap state, revision).
/// If more than one row somehow exists for a doc, the earliest (lowest id) wins
/// deterministically.
// ponytail: a first-write race could create two rows for a doc; the earliest-id
// tie-break keeps reads deterministic. Harden with a keyed put or a lock if it
// matters (rung 2).
fn load(doc: &str) -> Doc {
    let mut entries = records::find_by(DOCS, "doc", &json!(doc).to_string()).unwrap_or_default();
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    match entries.into_iter().next() {
        Some(e) => {
            let d = serde_json::from_str::<Value>(&e.data).unwrap_or_else(|_| json!({}));
            Doc {
                id: Some(e.id),
                // `meta` = scalar fields (title, …) as an lwwmap; `body` = an
                // rga text sequence. Older single-`state` rows read as meta.
                meta: d["meta"]
                    .as_str()
                    .or_else(|| d["state"].as_str())
                    .map(String::from)
                    .unwrap_or_else(crdt::lwwmap_new),
                body: d["body"].as_str().map(String::from).unwrap_or_else(crdt::rga_new),
                rev: e.revision,
            }
        }
        None => Doc { id: None, meta: crdt::lwwmap_new(), body: crdt::rga_new(), rev: 0 },
    }
}

/// A loaded document: the two CRDT states + the record id/revision.
struct Doc {
    id: Option<String>,
    meta: String,
    body: String,
    rev: u64,
}

fn record_data(doc: &str, meta: &str, body: &str) -> String {
    json!({ "doc": doc, "meta": meta, "body": body }).to_string()
}

/// The merged scalar fields (title, …) for an lwwmap meta state.
fn merged_doc(meta: &str) -> Value {
    crdt::value(meta).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| json!({}))
}

/// The current body text from the rga state.
fn body_text(body: &str) -> String {
    crdt::value(body)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

/// The full document view: scalar fields + `body` text + `body_elems` (the rga
/// `{id, ch}` list the client maps cursor positions against for id-anchored ops).
fn doc_json(doc: &str, d: &Doc) -> Value {
    let mut fields = merged_doc(&d.meta);
    if let Some(obj) = fields.as_object_mut() {
        obj.insert("body".to_string(), Value::String(body_text(&d.body)));
    }
    let elems: Value = crdt::rga_elements(&d.body)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!([]));
    json!({ "doc": doc, "rev": d.rev, "fields": fields, "body_elems": elems })
}

/// A scalar field's value as plain text (for diffing).
fn field_text(meta: &str, field: &str) -> String {
    match merged_doc(meta).get(field) {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

fn get_doc(doc: &str) -> Outcome {
    Outcome::Json(200, doc_json(doc, &load(doc)).to_string())
}

/// Apply one edit, merging it under optimistic concurrency (record revision as
/// the CAS token; on conflict reload + re-merge, safe because CRDT merge is
/// idempotent). The `body` field is an rga text sequence edited with id-anchored
/// insert/delete ops (so concurrent typing interleaves); every other field is a
/// last-writer-wins register.
fn apply_op(request: &IncomingRequest, doc: &str) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let field = body["field"].as_str().unwrap_or("").trim().to_string();
    if field.is_empty() {
        return Outcome::Err(422, "field required".into());
    }
    let replica = {
        let r = body["replica"].as_str().unwrap_or("").trim();
        if r.is_empty() {
            ids::short_code(8)
        } else {
            r.to_string()
        }
    };

    for _ in 0..MAX_RETRY {
        let cur = load(doc);
        // Compute the new (meta, body) pair + the (field, before, after) text for
        // history, depending on the op.
        let (new_meta, new_body, before, after) = match field.as_str() {
            "body" => {
                let before = body_text(&cur.body);
                let new_body = match apply_body_op(&cur.body, &body, &replica) {
                    Ok(s) => s,
                    Err(o) => return o,
                };
                let after = body_text(&new_body);
                (cur.meta.clone(), new_body, before, after)
            }
            _ => {
                let value = body.get("value").cloned().unwrap_or(Value::Null);
                if value.to_string().len() > MAX_FIELD_LEN {
                    return Outcome::Err(422, "value too long".into());
                }
                let ts = body["ts"].as_u64().unwrap_or_else(|| now() * 1000);
                let before = field_text(&cur.meta, &field);
                let new_meta =
                    match crdt::lwwmap_set(&cur.meta, &field, &value.to_string(), ts, &replica) {
                        Ok(s) => s,
                        Err(e) => return crdt_err(e),
                    };
                let after = field_text(&new_meta, &field);
                (new_meta, cur.body.clone(), before, after)
            }
        };

        let rec = record_data(doc, &new_meta, &new_body);
        let res = match &cur.id {
            Some(id) => records::update(DOCS, id, &rec, cur.rev),
            None => records::create(DOCS, &rec, &["doc".to_string()]),
        };
        match res {
            Ok(entry) => {
                // History only when the value actually changed (a losing LWW op,
                // or an idempotent re-delete, leaves before == after).
                if before != after {
                    record_history(doc, entry.revision, &field, &replica, &before, &after);
                }
                let d = Doc { id: cur.id, meta: new_meta, body: new_body, rev: entry.revision };
                return Outcome::Json(200, doc_json(doc, &d).to_string());
            }
            Err(records::StoreError::RevisionConflict(_)) => continue,
            Err(e) => return store_err(e),
        }
    }
    Outcome::Err(409, "too much contention, retry".into())
}

/// Apply an id-anchored rga op to the body state. `kind` is `insert`
/// (`{after, text, ts, seq}`) or `delete` (`{ids}`).
#[allow(clippy::result_large_err)]
fn apply_body_op(body_state: &str, req: &Value, replica: &str) -> Result<String, Outcome> {
    match req["kind"].as_str().unwrap_or("") {
        "insert" => {
            let after = req["after"].as_str().unwrap_or("");
            let text = req["text"].as_str().unwrap_or("");
            if text.is_empty() {
                return Err(Outcome::Err(422, "insert text required".into()));
            }
            if text.len() > MAX_FIELD_LEN {
                return Err(Outcome::Err(422, "insert too long".into()));
            }
            // Build a globally-unique, sortable id-base: ts dominates, so later
            // edits sort first among same-anchor siblings; replica + seq break
            // ties. The client mints the SAME shape so it can predict ids.
            let ts = req["ts"].as_u64().unwrap_or_else(|| now() * 1000);
            let seq = req["seq"].as_u64().unwrap_or(0);
            let id_base = format!("{ts:013}-{replica}-{seq:06}");
            crdt::rga_insert_after(body_state, after, text, &id_base).map_err(crdt_err)
        }
        "delete" => {
            let del_ids: Vec<String> = req["ids"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            crdt::rga_delete_ids(body_state, &del_ids).map_err(crdt_err)
        }
        other => Err(Outcome::Err(422, format!("unknown body op: {other}"))),
    }
}

// ---- history (composes diff:text) --------------------------------------------

// ponytail: history grows unbounded (one row per real change); fine for a demo.
// Cap it (keep last N) or roll up if a doc sees heavy editing.
fn record_history(doc: &str, rev: u64, field: &str, replica: &str, before: &str, after: &str) {
    let h = json!({
        "doc": doc, "rev": rev, "field": field, "replica": replica,
        "at": now(), "before": before, "after": after,
    });
    let _ = records::create(HISTORY, &h.to_string(), &["doc".to_string()]);
}

/// Per-revision history, newest first: each entry carries a unified diff (from
/// `diff:text`) of what that edit changed in the field.
fn get_history(doc: &str, path: &str) -> Outcome {
    let limit = query_i64(path, "limit").unwrap_or(25).clamp(1, 200) as usize;
    let mut rows: Vec<Value> = records::find_by(HISTORY, "doc", &json!(doc).to_string())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .collect();
    // newest first by revision
    rows.sort_by(|a, b| b["rev"].as_u64().unwrap_or(0).cmp(&a["rev"].as_u64().unwrap_or(0)));
    rows.truncate(limit);

    let entries: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let field = r["field"].as_str().unwrap_or("");
            let before = r["before"].as_str().unwrap_or("");
            let after = r["after"].as_str().unwrap_or("");
            let rev = r["rev"].as_u64().unwrap_or(0);
            let diff = textdiff::unified(
                before,
                after,
                &format!("{field}@r{}", rev.saturating_sub(1)),
                &format!("{field}@r{rev}"),
                1,
            );
            json!({
                "rev": rev,
                "field": field,
                "replica": r["replica"],
                "at": r["at"],
                "diff": diff,
            })
        })
        .collect();
    Outcome::Json(200, json!({ "doc": doc, "history": entries }).to_string())
}

// ---- presence ----------------------------------------------------------------

fn heartbeat(request: &IncomingRequest, doc: &str) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let user = body["user"].as_str().unwrap_or("").trim().to_string();
    if user.is_empty() {
        return Outcome::Err(422, "user required".into());
    }
    let existing = records::find_by(PRESENCE, "doc", &json!(doc).to_string()).unwrap_or_default();
    let mine = existing.into_iter().find(|e| {
        serde_json::from_str::<Value>(&e.data)
            .ok()
            .and_then(|d| d["user"].as_str().map(|u| u == user))
            .unwrap_or(false)
    });
    let data = json!({ "doc": doc, "user": user, "at": now() });
    match mine {
        Some(e) => {
            let _ = records::update(PRESENCE, &e.id, &data.to_string(), 0);
        }
        None => {
            let _ = records::create(PRESENCE, &data.to_string(), &["doc".to_string()]);
        }
    }
    Outcome::Json(200, json!({ "ok": true }).to_string())
}

fn presence(doc: &str) -> Outcome {
    let cutoff = now().saturating_sub(PRESENCE_WINDOW);
    let entries = records::find_by(PRESENCE, "doc", &json!(doc).to_string()).unwrap_or_default();
    let mut online: Vec<String> = entries
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .filter(|d| d["at"].as_u64().unwrap_or(0) >= cutoff)
        .filter_map(|d| d["user"].as_str().map(String::from))
        .collect();
    online.sort();
    online.dedup();
    Outcome::Json(200, json!({ "online": online }).to_string())
}

// ---- the SSE stream ----------------------------------------------------------

/// Hold the connection open and push the merged document whenever its revision
/// changes. Sends the current document immediately (so a late joiner is caught
/// up), then loops until the client disconnects or the cap is hit.
fn stream_events(response_out: ResponseOutparam, doc: &str, path: &str) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"text/event-stream".to_vec()]);
    let _ = headers.set("cache-control", &[b"no-cache".to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(200);
    let body = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));

    // start below any client-supplied rev so the first tick always pushes.
    let mut cursor = query_i64(path, "rev").unwrap_or(-1);

    {
        let stream = body.write().expect("write stream");
        if !write_all(&stream, b": connected\n\n") {
            return;
        }
        for _ in 0..MAX_TICKS {
            let d = load(doc);
            let frame = if (d.rev as i64) != cursor {
                cursor = d.rev as i64;
                format!("data: {}\n\n", doc_json(doc, &d))
            } else {
                ": ping\n\n".to_string()
            };
            if !write_all(&stream, frame.as_bytes()) {
                break; // client disconnected
            }
            monotonic_clock::subscribe_duration(POLL_MS * 1_000_000).block();
        }
    }
    let _ = OutgoingBody::finish(body, None);
}

// ---- http plumbing -----------------------------------------------------------

fn crdt_err(e: crdt::CrdtError) -> Outcome {
    match e {
        crdt::CrdtError::InvalidJson(m) => Outcome::Err(422, m),
        crdt::CrdtError::InvalidState(m) => Outcome::Err(500, m),
        crdt::CrdtError::TypeMismatch(m) => Outcome::Err(500, m),
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

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let body = request.consume().map_err(|_| ())?;
    let stream = body.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                // A ceiling, not a policy: past this the read stops and the caller
                // is told, rather than growing until the store's memory cap traps
                // the component and the connection just closes.
                if buf.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err(());
                }
                buf.extend_from_slice(&chunk);
            }
            // `Closed` is how wasi:io says end-of-body; `LastOperationFailed` is a
            // read that went wrong. Collapsing both into `break` returns a TRUNCATED
            // body as if it were complete — the same silent truncation that, on the
            // write side, took four runs to find.
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            Err(_) => return Err(()),
        }
    }
    Ok(buf)
}

fn query_i64(path: &str, key: &str) -> Option<i64> {
    let query = path.split('?').nth(1)?;
    query.split('&').find_map(|pair| {
        let mut it = pair.splitn(2, '=');
        (it.next()? == key).then(|| it.next().unwrap_or("").parse().ok())?
    })
}

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
            let _ = write_all(&stream, chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
