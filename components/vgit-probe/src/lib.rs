//! `vgit-probe` — an instrument for `vgit:store` (see wit/probe.wit).
//!
//!   POST /commit   {base, message, changes:[{path, content, remove}]} -> {commit, tree}
//!   GET  /read     ?commit=&path=
//!   GET  /paths    ?commit=
//!   GET  /diff     ?before=&after=
//!   GET  /tree     ?commit=&path=   the subtree id at a path, for reuse checks
//!   POST /ref      ?name=&to=[&expect=]
//!   GET  /ref      ?name=

#[allow(warnings)]
mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::vgit::store::{objects, refs, worktree};
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde_json::json;

struct Component;

fn param(query: &str, key: &str) -> String {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| percent(v))
        .unwrap_or_default()
}

fn percent(s: &str) -> String {
    let b = s.replace('+', " ");
    let b = b.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1), b.get(i + 2)) {
            (b'%', Some(h), Some(l)) => {
                match u8::from_str_radix(core::str::from_utf8(&[*h, *l]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            _ => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn err(e: objects::GitError) -> String {
    let (kind, msg) = match e {
        objects::GitError::NotFound(m) => ("not-found", m),
        objects::GitError::Corrupt(m) => ("corrupt", m),
        objects::GitError::Unavailable(m) => ("unavailable", m),
        objects::GitError::Invalid(m) => ("invalid", m),
    };
    json!({ "error": kind, "detail": msg }).to_string()
}

/// A ceiling on a request body, not a policy: past this the read gives up and
/// the body reads as empty, rather than growing until the store's memory cap
/// traps the component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

fn read_body(request: IncomingRequest) -> String {
    let Ok(body) = request.consume() else { return String::new() };
    let Ok(stream) = body.stream() else { return String::new() };
    let mut out = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                // Same reasoning as the error arm below: an over-long body reads
                // as empty rather than as a plausible prefix of itself.
                if out.len() + chunk.len() > MAX_BODY_BYTES {
                    return String::new();
                }
                out.extend_from_slice(&chunk);
            }
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            // No error channel here, so the choice is a truncated body or none.
            // None: a caller parsing an empty body fails cleanly, where half a
            // JSON document can parse into something plausible and wrong.
            Err(_) => return String::new(),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Walk to the subtree id at `path`, so a test can prove an untouched subtree was
/// reused by id rather than rewritten to an equal-looking one.
fn subtree(commit: &str, path: &str) -> Result<Option<String>, objects::GitError> {
    let info = objects::read_commit(commit)?;
    let mut id = info.tree;
    if path.is_empty() {
        return Ok(Some(id));
    }
    for seg in path.split('/') {
        let entries = objects::read_tree(&id)?;
        match entries.into_iter().find(|e| e.name == seg) {
            Some(e) => id = e.id,
            None => return Ok(None),
        }
    }
    Ok(Some(id))
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let (route, query) = match path.split_once('?') {
            Some((r, q)) => (r.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let method = request.method();

        let body = match (&method, route.as_str()) {
            (Method::Post, "/commit") => {
                let raw = read_body(request);
                let v: serde_json::Value =
                    serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
                let changes: Vec<worktree::PathChange> = v["changes"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|c| worktree::PathChange {
                        path: c["path"].as_str().unwrap_or_default().to_string(),
                        content: c["content"].as_str().unwrap_or_default().as_bytes().to_vec(),
                        mode: c["mode"].as_str().unwrap_or_default().to_string(),
                        remove: c["remove"].as_bool().unwrap_or(false),
                    })
                    .collect();
                let base = v["base"].as_str().unwrap_or_default().to_string();
                let message = v["message"].as_str().unwrap_or("change").to_string();
                // A fixed author and a fixed time: the test compares this id with
                // one `git commit-tree` produces from the same inputs, and either
                // moving would make that impossible.
                match worktree::commit_changes(
                    &base,
                    &changes,
                    "Ada <ada@example.com>",
                    1_700_000_000,
                    &message,
                ) {
                    Ok(c) => match objects::read_commit(&c) {
                        Ok(info) => json!({ "commit": c, "tree": info.tree }).to_string(),
                        Err(e) => err(e),
                    },
                    Err(e) => err(e),
                }
            }
            (Method::Get, "/read") => {
                match worktree::read_path(&param(&query, "commit"), &param(&query, "path")) {
                    Ok(Some(b)) => json!({ "content": String::from_utf8_lossy(&b) }).to_string(),
                    Ok(None) => json!({ "found": false }).to_string(),
                    Err(e) => err(e),
                }
            }
            (Method::Get, "/paths") => {
                match worktree::list_paths(&param(&query, "commit"), &param(&query, "prefix")) {
                    Ok(p) => json!({ "paths": p }).to_string(),
                    Err(e) => err(e),
                }
            }
            (Method::Get, "/diff") => {
                match worktree::diff(&param(&query, "before"), &param(&query, "after")) {
                    Ok(d) => json!({
                        "changes": d.iter().map(|c| json!({"path": c.path, "kind": c.kind}))
                            .collect::<Vec<_>>()
                    })
                    .to_string(),
                    Err(e) => err(e),
                }
            }
            (Method::Get, "/tree") => {
                match subtree(&param(&query, "commit"), &param(&query, "path")) {
                    Ok(Some(id)) => json!({ "tree": id }).to_string(),
                    Ok(None) => json!({ "found": false }).to_string(),
                    Err(e) => err(e),
                }
            }
            (Method::Post, "/ref") => {
                let expect = param(&query, "expect");
                let expect = if expect.is_empty() { None } else { Some(expect) };
                match refs::update(&param(&query, "name"), expect.as_deref(), &param(&query, "to"))
                {
                    Ok(won) => json!({ "updated": won }).to_string(),
                    Err(e) => err(e),
                }
            }
            (Method::Get, "/ref") => match refs::read(&param(&query, "name")) {
                Ok(Some(sha)) => json!({ "ref": sha }).to_string(),
                Ok(None) => json!({ "found": false }).to_string(),
                Err(e) => err(e),
            },
            _ => json!({
                "service": "vgit-probe",
                "routes": ["/commit", "/read", "/paths", "/diff", "/tree", "/ref"]
            })
            .to_string(),
        };

        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"application/json".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(200);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            for chunk in body.as_bytes().chunks(4096) {
                let _ = stream.blocking_write_and_flush(chunk);
            }
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
