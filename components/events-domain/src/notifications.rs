//! The stream takes a ticket: `EventSource` cannot set `Authorization`, and a query bearer lands in logs.

use serde_json::json;

use crate::bindings::notify::inbox::inbox;
use crate::bindings::notify::prefs::preferences as prefs;
use crate::bindings::p3::http::types::{ErrorCode, Fields, Method, Response};
use crate::{require, Reply, Route};

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

fn channels(v: &serde_json::Value) -> Vec<prefs::Channel> {
    v.as_array()
        .map(|a| a.iter().filter_map(|c| c.as_str().and_then(channel_of)).collect())
        .unwrap_or_default()
}

/// Config, not a secret: it signs a 60-second read-only view of one person's own notifications.
fn ticket_secret() -> String {
    crate::bindings::wasi::config::store::get("stream-ticket-secret")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        // Named so a config dump shows nobody set it.
        .unwrap_or_else(|| "unset-stream-ticket-secret".to_string())
}

const TICKET_TTL: u64 = 60;

fn ticket_body(subject: &str, expires: u64) -> String {
    format!("{subject}.{expires}")
}

/// Signed over subject and expiry, so neither can be edited.
fn mint_ticket(subject: &str) -> String {
    let expires = crate::remind::now() + TICKET_TTL;
    let body = ticket_body(subject, expires);
    match crate::bindings::webhook::sign::signer::sign(
        body.as_bytes(),
        &ticket_secret(),
        crate::bindings::webhook::sign::signer::Scheme::Github,
    ) {
        Ok(sig) => format!("{subject}.{expires}.{}", sig.header),
        Err(_) => String::new(),
    }
}

fn redeem_ticket(ticket: &str) -> Option<String> {
    let (rest, header) = ticket.rsplit_once('.')?;

    let (subject, expires) = rest.rsplit_once('.')?;
    if expires.parse::<u64>().ok()? < crate::remind::now() {
        return None;
    }
    crate::bindings::webhook::sign::signer::verify(
        rest.as_bytes(),
        header,
        &ticket_secret(),
        crate::bindings::webhook::sign::signer::Scheme::Github,
        TICKET_TTL * 10,
    )
    .ok()?;
    Some(subject.to_string())
}

fn stream_ticket(route: &Route) -> Reply {
    let p = match require(route, "ticket", "read") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let t = mint_ticket(&p.subject);
    if t.is_empty() {
        return Reply::err(500, "could_not_sign");
    }
    Reply::json(200, json!({ "ticket": t, "ttl_seconds": TICKET_TTL }))
}

pub fn stream_subject(route: &Route) -> Option<String> {
    redeem_ticket(&route.param("ticket"))
}

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Get, ["api", "notifications"]) => list(route),
        (Method::Get, ["api", "notifications", "unread"]) => unread(route),
        (Method::Post, ["api", "notifications", "read"]) => read(route, body),
        (Method::Post, ["api", "notifications", "stream-ticket"]) => stream_ticket(route),
        (Method::Get, ["api", "prefs"]) => get_prefs(route),
        (Method::Put, ["api", "prefs"]) => put_prefs(route, body),
        _ => Reply::err(404, "not_found"),
    }
}

fn list(route: &Route) -> Reply {
    let p = match require(route, "ticket", "read") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let after = route.param("after").parse::<u64>().unwrap_or(0);
    match inbox::since(&p.subject, after, 50) {
        Ok(notes) => Reply::json(
            200,
            json!({
                "notifications": notes.iter().map(|n| json!({
                    "seq": n.seq, "kind": n.kind, "title": n.title,
                    "body": n.body, "payload": n.payload, "at": n.at, "read": n.read,
                })).collect::<Vec<_>>()
            }),
        ),
        Err(e) => Reply::err(500, &format!("inbox: {e:?}")),
    }
}

fn unread(route: &Route) -> Reply {
    let p = match require(route, "ticket", "read") {
        Ok(p) => p,
        Err(r) => return r,
    };
    match inbox::unread_count(&p.subject) {
        Ok(n) => Reply::json(200, json!({ "unread": n })),
        Err(e) => Reply::err(500, &format!("inbox: {e:?}")),
    }
}

fn read(route: &Route, body: &str) -> Reply {
    let p = match require(route, "ticket", "read") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let input: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let r = if input.get("seqs").is_some() {
        let seqs: Vec<u64> = input["seqs"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_u64()).collect())
            .unwrap_or_default();
        inbox::mark_read(&p.subject, &seqs)
    } else {
        inbox::mark_all_read(&p.subject, input["through"].as_u64().unwrap_or(0))
    };
    match r {
        Ok(n) => Reply::json(200, json!({ "marked": n })),
        Err(e) => Reply::err(500, &format!("inbox: {e:?}")),
    }
}

fn get_prefs(route: &Route) -> Reply {
    let p = match require(route, "ticket", "read") {
        Ok(p) => p,
        Err(r) => return r,
    };
    match prefs::get(&p.subject) {
        Ok(pref) => Reply::json(
            200,
            json!({
                "default_channels": pref.default_channels.iter().map(|c| channel_name(*c)).collect::<Vec<_>>(),
                "email_address": pref.email_address,
                "overrides": pref.overrides.iter().map(|(k, v)| {
                    (k.clone(), json!(v.iter().map(|c| channel_name(*c)).collect::<Vec<_>>()))
                }).collect::<serde_json::Map<_, _>>(),
            }),
        ),
        Err(e) => Reply::err(500, &format!("prefs: {e:?}")),
    }
}

fn put_prefs(route: &Route, body: &str) -> Reply {
    let p = match require(route, "ticket", "read") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let Ok(input) = serde_json::from_str::<serde_json::Value>(body) else {
        return Reply::err(400, "malformed_body");
    };
    // The caller's subject, never the body's, or anyone could rewrite anyone's email address.
    let pref = prefs::Preference {
        subject: p.subject.clone(),
        default_channels: channels(&input["default_channels"]),
        overrides: input["overrides"]
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), channels(v))).collect())
            .unwrap_or_default(),
        email_address: input["email_address"].as_str().unwrap_or_default().to_string(),
    };
    match prefs::put(&pref) {
        Ok(()) => Reply::json(200, json!({ "ok": true })),
        Err(e) => Reply::err(400, &format!("prefs: {e:?}")),
    }
}

/// A never-returning invocation cannot be reclaimed, so the stream ends and `EventSource` reopens.
const MAX_TICKS: u32 = 300;
const POLL_MS: u64 = 1000;

pub fn stream(route: &Route) -> Result<Response, ErrorCode> {
    let Some(subject) = stream_subject(route) else {
        // 401, not a 200 with an error frame: `EventSource` would reconnect forever.
        return crate::respond(401, "application/json", br#"{"error":"bad_ticket"}"#.to_vec());
    };

    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"text/event-stream".to_vec()]);
    let _ = headers.set("cache-control", &[b"no-cache".to_vec()]);

    let mut cursor = route.param("after").parse::<u64>().unwrap_or(0);
    // `write_all` returns what the reader did not take: non-empty means the client left.
    let (mut out, rx) = crate::bindings::wit_stream::new();
    wit_bindgen::spawn_local(async move {
        if !out.write_all(b": connected\n\n".to_vec()).await.is_empty() {
            return;
        }
        for _ in 0..MAX_TICKS {
            let notes = inbox::since(&subject, cursor, 50).unwrap_or_default();
            let frame = if notes.is_empty() {
                // A comment keeps proxy idle timeouts away without the browser seeing an event.
                ": ping\n\n".to_string()
            } else {
                notes
                    .iter()
                    .map(|n| {
                        cursor = cursor.max(n.seq);
                        format!(
                            "data: {}\n\n",
                            json!({
                                "seq": n.seq, "kind": n.kind, "title": n.title,
                                "body": n.body, "at": n.at, "read": n.read,
                            })
                        )
                    })
                    .collect::<String>()
            };
            if !out.write_all(frame.into_bytes()).await.is_empty() {
                break;
            }
            crate::bindings::p3::clocks::monotonic_clock::wait_for(POLL_MS * 1_000_000).await;
        }
    });
    let (trailers_tx, trailers_rx) = crate::bindings::wit_future::new(|| Ok(None));
    drop(trailers_tx);
    let (response, _sent) = Response::new(headers, Some(rx), trailers_rx);
    let _ = response.set_status_code(200);
    Ok(response)
}
