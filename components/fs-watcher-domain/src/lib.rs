//! `fs-watcher-domain` — watch a directory for changes and report them over HTTP

#[allow(warnings)]
mod bindings;
use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::os::fs::watcher;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use bindings::wasi::keyvalue::store;
use serde_json::json;

struct Component;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        if let Ok(bucket) = store::open("default") {
            let count_bytes = bucket.get("usage_count").unwrap_or(None).unwrap_or(b"0".to_vec());
            let count = String::from_utf8_lossy(&count_bytes).parse::<u64>().unwrap_or(0);
            let _ = bucket.set("usage_count", (count + 1).to_string().as_bytes());
        }
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let query: std::collections::HashMap<String, String> = path
            .split_once('?')
            .map(|(_, q)| {
                q.split('&')
                    .filter_map(|kv| kv.split_once('='))
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => Outcome::Html(200, r#"<!DOCTYPE html><html><head><title>FS Watcher</title></head><body><h1>FS Watcher</h1><button onclick="fetch('/api/watch').then(r=>r.json()).then(d=>document.getElementById('r').innerText=JSON.stringify(d))">Watch Directory</button><div id="r"></div></body></html>"#.to_string()),
            // The cursor is the caller's, not ours: an empty one means "start
            // from now" and the answer carries the position to send back. Keeping
            // it server-side would make two browsers polling this share a
            // position and each miss what the other consumed.
            (Method::Get, ["api", "watch"]) => {
                let cursor = query.get("cursor").map(String::as_str).unwrap_or_default();
                match watcher::poll("/var/log", cursor) {
                    Ok(changes) => Outcome::Json(
                        200,
                        json!({
                            "events": changes.events.iter().map(|e| json!({
                                "path": e.path,
                                "kind": match e.kind {
                                    watcher::Change::Created => "created",
                                    watcher::Change::Modified => "modified",
                                    watcher::Change::Removed => "removed",
                                },
                                "at": e.at,
                            })).collect::<Vec<_>>(),
                            "cursor": changes.cursor,
                            "truncated": changes.truncated,
                        })
                        .to_string(),
                    ),
                    // The three errors are three different answers. A refusal is
                    // 403 and not worth retrying; a missing directory is 404; a
                    // watcher that is down is 503 and is.
                    Err(watcher::WatchError::NotPermitted(d)) => Outcome::Err(403, d),
                    Err(watcher::WatchError::NoSuchDirectory(d)) => Outcome::Err(404, d),
                    Err(watcher::WatchError::Unavailable(d)) => Outcome::Err(503, d),
                }
            },
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Html(u16, String),
    Json(u16, String),
    Err(u16, String),
}

fn emit(response_out: ResponseOutparam, result: Outcome) {
    let (code, body, content_type) = match result {
        Outcome::Html(c, b) => (c, b, b"text/html".to_vec()),
        Outcome::Json(c, b) => (c, b, b"application/json".to_vec()),
        Outcome::Err(c, m) => (c, json!({ "error": m }).to_string(), b"application/json".to_vec()),
    };
    let headers = Fields::new();
    let _ = headers.set("content-type", &[content_type]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(code);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    let bytes = body.as_bytes();
    if !bytes.is_empty() {
        let stream = out.write().expect("write stream");
        let _ = write_all(&stream, bytes);
    }
    let _ = OutgoingBody::finish(out, None);
}
bindings::export!(Component with_types_in bindings);

guestio::guest_write_all!();
