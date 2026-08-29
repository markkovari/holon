//! `notify-probe` — the door onto `notify:prefs` and `notify:inbox`.
//!
//!   PUT  /prefs                  {subject, default_channels[], overrides{}, email_address}
//!   GET  /prefs?subject=
//!   POST /notify                 {subject, kind, title, body, payload}
//!   GET  /inbox?subject=&after=&limit=
//!   GET  /unread?subject=
//!   POST /read                   {subject, seqs[]}   or {subject, through}
//!
//! Every route answers JSON with 200 unless the request itself was malformed. What
//! is under test is what the capabilities decided, and a status code would flatten
//! "that subject wants no email" into the same shape as "the gateway refused".

#[allow(warnings)]
mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::notify::inbox::inbox;
use bindings::notify::prefs::preferences as prefs;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde_json::{json, Value};

guestio::guest_write_all!();

struct Component;

fn channel_name(c: prefs::Channel) -> &'static str {
    match c {
        prefs::Channel::InApp => "in-app",
        prefs::Channel::Email => "email",
    }
}

fn channel_of(s: &str) -> Option<prefs::Channel> {
    match s {
        "in-app" => Some(prefs::Channel::InApp),
        "email" => Some(prefs::Channel::Email),
        _ => None,
    }
}

fn channels(v: &Value) -> Vec<prefs::Channel> {
    v.as_array()
        .map(|a| a.iter().filter_map(|c| c.as_str().and_then(channel_of)).collect())
        .unwrap_or_default()
}

fn param(query: &str, key: &str) -> String {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.replace("%40", "@").replace('+', " "))
        .unwrap_or_default()
}

const MAX_BODY_BYTES: usize = 1 << 20;

guestio::guest_read_body_text!(MAX_BODY_BYTES);

fn note_json(n: &inbox::Note) -> Value {
    json!({
        "seq": n.seq, "kind": n.kind, "title": n.title, "body": n.body,
        "payload": n.payload, "at": n.at, "read": n.read,
    })
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let target = request.path_with_query().unwrap_or_else(|| "/".into());
        let (path, query) = match target.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (target.clone(), String::new()),
        };
        let method = request.method();
        let raw = match method {
            Method::Post | Method::Put => read_body(&request),
            _ => String::new(),
        };
        let body: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);

        let out = match (&method, path.as_str()) {
            // gate-lib waits on this before it asks anything else.
            (_, "/health") => json!({"ok": true}),
            (_, "/") => json!({
                "probe": "notify",
                "routes": ["PUT /prefs", "GET /prefs?subject=", "POST /notify",
                           "GET /inbox?subject=&after=&limit=", "GET /unread?subject=",
                           "POST /read"]
            }),

            (Method::Put, "/prefs") => {
                let overrides: Vec<(String, Vec<prefs::Channel>)> = body["overrides"]
                    .as_object()
                    .map(|m| m.iter().map(|(k, v)| (k.clone(), channels(v))).collect())
                    .unwrap_or_default();
                let p = prefs::Preference {
                    subject: body["subject"].as_str().unwrap_or_default().to_string(),
                    default_channels: channels(&body["default_channels"]),
                    overrides,
                    email_address: body["email_address"].as_str().unwrap_or_default().to_string(),
                };
                match prefs::put(&p) {
                    Ok(()) => json!({"ok": true}),
                    Err(e) => json!({"error": format!("{e:?}")}),
                }
            }

            (Method::Get, "/prefs") => match prefs::get(&param(&query, "subject")) {
                Ok(p) => json!({
                    "subject": p.subject,
                    "default_channels": p.default_channels.iter().map(|c| channel_name(*c)).collect::<Vec<_>>(),
                    "overrides": p.overrides.iter().map(|(k, v)| {
                        (k.clone(), json!(v.iter().map(|c| channel_name(*c)).collect::<Vec<_>>()))
                    }).collect::<serde_json::Map<_, _>>(),
                    "email_address": p.email_address,
                }),
                Err(e) => json!({"error": format!("{e:?}")}),
            },

            (Method::Post, "/notify") => {
                let r = prefs::notify(
                    body["subject"].as_str().unwrap_or_default(),
                    body["kind"].as_str().unwrap_or_default(),
                    body["title"].as_str().unwrap_or_default(),
                    body["body"].as_str().unwrap_or_default(),
                    body["payload"].as_str().unwrap_or_default(),
                );
                match r {
                    Ok(outcomes) => json!({
                        "outcomes": outcomes.iter().map(|o| json!({
                            "channel": channel_name(o.channel), "ok": o.ok, "detail": o.detail,
                        })).collect::<Vec<_>>()
                    }),
                    Err(e) => json!({"error": format!("{e:?}")}),
                }
            }

            (Method::Get, "/inbox") => {
                let after = param(&query, "after").parse::<u64>().unwrap_or(0);
                let limit = param(&query, "limit").parse::<u32>().unwrap_or(50);
                match inbox::since(&param(&query, "subject"), after, limit) {
                    Ok(notes) => {
                        json!({"notes": notes.iter().map(note_json).collect::<Vec<_>>()})
                    }
                    Err(e) => json!({"error": format!("{e:?}")}),
                }
            }

            (Method::Get, "/unread") => match inbox::unread_count(&param(&query, "subject")) {
                Ok(n) => json!({"unread": n}),
                Err(e) => json!({"error": format!("{e:?}")}),
            },

            (Method::Post, "/read") => {
                let subject = body["subject"].as_str().unwrap_or_default();
                let r = if let Some(t) = body["through"].as_u64() {
                    inbox::mark_all_read(subject, t)
                } else {
                    let seqs: Vec<u64> = body["seqs"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_u64()).collect())
                        .unwrap_or_default();
                    inbox::mark_read(subject, &seqs)
                };
                match r {
                    Ok(n) => json!({"marked": n}),
                    Err(e) => json!({"error": format!("{e:?}")}),
                }
            }

            _ => json!({"error": "not_found"}),
        };

        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"application/json".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(200);
        let ob = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = ob.write() {
            let _ = write_all(&stream, out.to_string().as_bytes());
            drop(stream);
        }
        let _ = OutgoingBody::finish(ob, None);
    }
}

bindings::export!(Component with_types_in bindings);
