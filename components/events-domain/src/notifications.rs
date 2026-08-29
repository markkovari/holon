//! The person's own view of their notifications: the badge, the list, and the tail.
//!
//! ## The stream is served here, not by the capability
//!
//! `notify:inbox` holds notes and hands them back by cursor; it does not hold an
//! HTTP connection, because a capability that owned a socket would only work inside
//! an app shaped the way the capability expected. So the SSE endpoint lives in the
//! app, and it is a loop over `since(after)` — the same shape `flags-domain` uses.
//!
//! ## Why the stream needs a ticket
//!
//! `EventSource` cannot set an `Authorization` header. The bearer therefore cannot
//! travel the way it does everywhere else, and putting it in the query string would
//! write a live session token into every access log and `Referer`. Instead an
//! authenticated POST mints a short-lived, single-subject ticket signed by
//! `webhook:sign`, and the GET presents that. A leaked 60-second ticket for one
//! person's notification feed is a much smaller thing than a leaked bearer.

use serde_json::json;

use crate::bindings::notify::inbox::inbox;
use crate::bindings::notify::prefs::preferences as prefs;
use crate::bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
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

/// The secret the stream ticket is signed with. Config, not a `comp:secrets`
/// reference, and that is a deliberate line: this signs a 60-second read-only
/// capability over one person's own notification list, not a payment. A secret by
/// reference is for things whose leak is a breach; this one's leak is a minute of
/// somebody else's notifications.
fn ticket_secret() -> String {
    crate::bindings::wasi::config::store::get("stream-ticket-secret")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        // A deployment that sets none still gets a working stream, signed with a
        // constant. Named so that it is obvious in a config dump that nobody set it.
        .unwrap_or_else(|| "unset-stream-ticket-secret".to_string())
}

/// Sixty seconds. Long enough for a browser to open the connection it just asked
/// for, short enough that a ticket in a log is worth nothing by the time anyone
/// reads it.
const TICKET_TTL: u64 = 60;

fn ticket_body(subject: &str, expires: u64) -> String {
    format!("{subject}.{expires}")
}

/// Mint one. The signature is over subject AND expiry, so neither can be edited
/// without invalidating it — a ticket for `ada` cannot be turned into one for `bob`
/// by changing the query string.
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

/// Whose stream is this, if the ticket is good?
fn redeem_ticket(ticket: &str) -> Option<String> {
    let mut parts = ticket.rsplitn(2, '.');
    let header = parts.next()?;
    let rest = parts.next()?;
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

/// `POST /api/notifications/stream-ticket` — authenticated the normal way.
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

/// Whose notifications a `?ticket=` names, or nobody.
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
    // `through: 0` from mark-all-read means everything, which is what a "mark all
    // read" button sends and what the capability already understands.
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
    // The SUBJECT is the caller's, never the body's. Taking it from the body would
    // let anyone rewrite anyone else's preferences — including the address their
    // email goes to.
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

// ---- the live tail ------------------------------------------------------------

/// How long a stream lives before the browser is asked to reconnect. A component
/// invocation that never returns is one the host can never reclaim, so the stream
/// ends politely and `EventSource` reopens — which is what it does natively.
const MAX_TICKS: u32 = 300;
const POLL_MS: u64 = 1000;

/// Hold the connection open and push each new note as an SSE frame.
///
/// This bypasses `Reply` entirely: everything else in this app builds a body and
/// hands it back, and a stream cannot — the response has to be SET before the first
/// frame is written, and then written to for as long as it lasts.
///
/// It reads `notify:inbox` by cursor. The capability holds no socket, so the
/// realtime half is the app's; what makes it cheap is that `since(after)` is exactly
/// "what is new", the same call a page uses to load.
pub fn stream(response_out: ResponseOutparam, route: &Route) {
    let Some(subject) = stream_subject(route) else {
        // A bad or expired ticket gets a 401 and no stream. Not a 200 with an error
        // frame: `EventSource` would reconnect forever against it.
        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"application/json".to_vec()]);
        let response = OutgoingResponse::new(headers);
        let _ = response.set_status_code(401);
        let body = response.body().expect("body");
        ResponseOutparam::set(response_out, Ok(response));
        if let Ok(s) = body.write() {
            let _ = crate::write_all(&s, br#"{"error":"bad_ticket"}"#);
            drop(s);
        }
        let _ = OutgoingBody::finish(body, None);
        return;
    };

    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"text/event-stream".to_vec()]);
    let _ = headers.set("cache-control", &[b"no-cache".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(200);
    let body = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));

    let mut cursor = route.param("after").parse::<u64>().unwrap_or(0);
    {
        let Ok(out) = body.write() else { return };
        if !crate::write_all(&out, b": connected\n\n") {
            return;
        }
        for _ in 0..MAX_TICKS {
            let notes = inbox::since(&subject, cursor, 50).unwrap_or_default();
            let frame = if notes.is_empty() {
                // A comment, not data: it keeps the connection alive through any
                // proxy with an idle timeout without the browser seeing an event.
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
            if !crate::write_all(&out, frame.as_bytes()) {
                break;
            }
            crate::bindings::wasi::clocks::monotonic_clock::subscribe_duration(
                POLL_MS * 1_000_000,
            )
            .block();
        }
    }
    let _ = OutgoingBody::finish(body, None);
}
