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
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../host/wit/deps/comp-secrets",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../mail-http/wit",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../notify-inbox/wit",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "../notify-prefs/wit",
            "wit",
        ],
        world: "notify:probe/notify-probe",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use bindings::notify::inbox::inbox;
use bindings::notify::prefs::preferences as prefs;
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};
use serde_json::{json, Value};

guestio::guest_p3_respond!();

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

guestio::guest_p3_read_body_text!(MAX_BODY_BYTES);

fn note_json(n: &inbox::Note) -> Value {
    json!({
        "seq": n.seq, "kind": n.kind, "title": n.title, "body": n.body,
        "payload": n.payload, "at": n.at, "read": n.read,
    })
}

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let target = request.get_path_with_query().unwrap_or_else(|| "/".into());
        let (path, query) = match target.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (target.clone(), String::new()),
        };
        let method = request.get_method();
        let raw = match method {
            Method::Post | Method::Put => read_body(request).await,
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

        respond(200, "application/json", out.to_string())
    }
}

bindings::export!(Component with_types_in bindings);
