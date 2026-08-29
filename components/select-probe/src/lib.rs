//! `select-probe` — an instrument for `graph:select` (see wit/probe.wit).
//!
//!   POST /select  {entries:[…]}                 — decide, without acting
//!   POST /land    {entries:[…], landing:{…}}    — decide, and propose the winner
//!
//! Both, because the assertion that matters most is a NEGATIVE one: when nothing
//! passed the gate, the forge must see no request at all. That is only checkable
//! against something that would have made one.

#[allow(warnings)]
mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::graph::select::selector as sel;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde_json::json;

struct Component;

/// A ceiling on a request body, not a policy: past this the read gives up and
/// the body reads as empty, rather than growing until the store's memory cap
/// traps the component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_read_body_text!(MAX_BODY_BYTES);

fn entries_of(v: &serde_json::Value) -> Vec<sel::Entry> {
    v["entries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|e| sel::Entry {
            branch: e["branch"].as_str().unwrap_or_default().to_string(),
            accepted: e["accepted"].as_bool().unwrap_or(false),
            score: e["score"].as_u64().unwrap_or(0) as u32,
            digest: e["digest"].as_str().unwrap_or_default().to_string(),
            spent_tokens: e["spent_tokens"].as_u64().unwrap_or(0) as u32,
            attempts: e["attempts"].as_u64().unwrap_or(0) as u32,
            files: e["files"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|f| sel::File {
                    path: f["path"].as_str().unwrap_or_default().to_string(),
                    content: f["content"].as_str().unwrap_or_default().to_string(),
                })
                .collect(),
        })
        .collect()
}

fn outcome_json(o: &sel::Outcome) -> serde_json::Value {
    let mut out = json!({
        "distinct": o.distinct,
        "accepted": o.accepted,
        "spent_tokens": o.spent_tokens,
    });
    match &o.decision {
        sel::Decision::Winner(c) => {
            out["winner"] = json!({ "index": c.index, "branch": c.branch, "because": c.because });
        }
        sel::Decision::NothingAcceptable(why) => {
            out["nothing_acceptable"] = json!(why);
        }
    }
    out
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();

        let body = match route.as_str() {
            "/select" => {
                let v: serde_json::Value =
                    serde_json::from_str(&read_body(&request)).unwrap_or(serde_json::Value::Null);
                match sel::select(&entries_of(&v)) {
                    Ok(o) => outcome_json(&o).to_string(),
                    Err(sel::SelectError::Invalid(m)) => {
                        json!({ "error": "invalid", "detail": m }).to_string()
                    }
                }
            }
            "/land" => {
                let v: serde_json::Value =
                    serde_json::from_str(&read_body(&request)).unwrap_or(serde_json::Value::Null);
                let l = &v["landing"];
                let landing = sel::Landing {
                    branch: l["branch"].as_str().unwrap_or("candidate").to_string(),
                    base: l["base"].as_str().unwrap_or_default().to_string(),
                    title: l["title"].as_str().unwrap_or("a candidate").to_string(),
                    body: l["body"].as_str().unwrap_or_default().to_string(),
                    message: l["message"].as_str().unwrap_or("a candidate").to_string(),
                };
                match sel::land(&entries_of(&v), &landing) {
                    Ok(o) => json!({
                        "number": o.number, "url": o.url, "commit": o.commit, "branch": o.branch,
                    })
                    .to_string(),
                    Err(e) => {
                        let (kind, detail) = match e {
                            sel::LandError::NothingAcceptable(m) => ("nothing-acceptable", m),
                            sel::LandError::Forge(m) => ("forge", m),
                            sel::LandError::Invalid(m) => ("invalid", m),
                        };
                        json!({ "error": kind, "detail": detail }).to_string()
                    }
                }
            }
            _ => json!({ "service": "select-probe", "routes": ["/select", "/land"] }).to_string(),
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
