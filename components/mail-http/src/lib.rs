//! `mail-http` — one email, POSTed to whatever gateway config names.
//!
//! ## Why HTTP and not SMTP
//!
//! `comp-host` wires no `wasi:sockets`, so a component cannot open a TCP connection
//! and therefore cannot speak SMTP at all. That is not a limitation worked around
//! here — it is the reason this interface is shaped as a POST, and the reason
//! `tools/mail-relay` exists: it is the piece that speaks real SMTP, on the outside,
//! where sockets are allowed.
//!
//! ## What is configured, and what is a secret
//!
//! `mail:gateway-url` and `mail:from` are config. The API key is a SECRET, read
//! through `comp:secrets` under `mail-api-key` — ADR-0051: a secret arrives by
//! reference, never as config. A gateway that needs no key (the local relay) simply
//! has none, and the Authorization header is then absent rather than empty.

#[allow(warnings)]
mod bindings;

use bindings::comp::secrets::reader as secrets;
use bindings::exports::mail::send::sender::{Email, Guest, SendError};
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};
use bindings::wasi::io::streams::StreamError;

guestio::guest_write_all!();

struct Component;

fn parse_url(url: &str) -> Result<(Scheme, String, String), SendError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(SendError::NotConfigured(format!("bad gateway url: {url}")));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].to_string()),
        None => (rest.to_string(), "/".to_string()),
    };
    Ok((scheme, authority, path))
}

/// The gateway's key, revealed only at the moment it is used.
///
/// `none` is not an error — `comp:secrets` says so, and it is the normal way to run
/// against a gateway that needs no credential, which is exactly the local relay.
fn api_key() -> Option<String> {
    match secrets::get("mail-api-key") {
        Ok(Some(s)) => secrets::reveal(&s).ok().filter(|v| !v.is_empty()),
        _ => None,
    }
}

fn cfg(key: &str) -> Option<String> {
    config::get(key).ok().flatten().filter(|s| !s.is_empty())
}

/// The gateway's own id for the message.
///
/// Resend answers `{"id":"..."}`; the relay answers the same shape with what MailHog
/// assigned. A gateway that returns neither is not an error — the send happened —
/// so the status stands in, and the caller gets something rather than an empty
/// string it has to special-case.
fn message_id(body: &[u8], status: u16) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("id")
                .or_else(|| v.get("message_id"))
                .and_then(|i| i.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| format!("accepted-{status}"))
}

impl Guest for Component {
    fn send(msg: Email) -> Result<String, SendError> {
        let url = cfg("mail:gateway-url").ok_or_else(|| {
            SendError::NotConfigured("no mail:gateway-url — nothing to send through".into())
        })?;
        let from = cfg("mail:from").ok_or_else(|| {
            SendError::NotConfigured("no mail:from — a gateway will refuse a message with no sender".into())
        })?;
        if !msg.to.contains('@') {
            return Err(SendError::Rejected(format!("not an address: {}", msg.to)));
        }

        // Resend's shape, which the relay also accepts. `text` rather than `html`:
        // an event notification has no markup, and a reader who cannot render HTML
        // still gets the message.
        let payload = serde_json::json!({
            "from": from,
            "to": [msg.to],
            "subject": msg.subject,
            "text": msg.body,
        })
        .to_string();

        let (scheme, authority, path) = parse_url(&url)?;
        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"application/json".to_vec()]);
        // Absent, not empty, when there is no key: a gateway that needs none (the
        // relay) would otherwise see `Authorization: Bearer ` and be entitled to
        // reject it.
        if let Some(key) = api_key() {
            let _ = headers.set("authorization", &[format!("Bearer {key}").into_bytes()]);
        }

        let req = OutgoingRequest::new(headers);
        let net = |c: &str| SendError::Unavailable(c.to_string());
        req.set_method(&Method::Post).map_err(|_| net("set method"))?;
        req.set_scheme(Some(&scheme)).map_err(|_| net("set scheme"))?;
        req.set_authority(Some(&authority)).map_err(|_| net("set authority"))?;
        req.set_path_with_query(Some(&path)).map_err(|_| net("set path"))?;
        {
            let out = req.body().map_err(|_| net("body"))?;
            {
                let stream = out.write().map_err(|_| net("write stream"))?;
                if !write_all(&stream, payload.as_bytes()) {
                    return Err(net("body write"));
                }
            }
            OutgoingBody::finish(out, None).map_err(|_| net("finish body"))?;
        }

        let future = outgoing_handler::handle(req, Some(RequestOptions::new()))
            .map_err(|e| SendError::Unavailable(format!("http handle: {e:?}")))?;
        future.subscribe().block();
        let resp = future
            .get()
            .ok_or_else(|| net("no response"))?
            .map_err(|_| net("response taken"))?
            .map_err(|e| SendError::Unavailable(format!("http: {e:?}")))?;

        let status = resp.status();
        let mut body = Vec::new();
        if let Ok(incoming) = resp.consume() {
            if let Ok(stream) = incoming.stream() {
                loop {
                    match stream.blocking_read(8192) {
                        Ok(c) if c.is_empty() => break,
                        Ok(c) => body.extend_from_slice(&c),
                        Err(StreamError::Closed) => break,
                        Err(_) => break,
                    }
                }
            }
        }
        let detail = String::from_utf8_lossy(&body).chars().take(200).collect::<String>();

        // The split that `notify:dispatch` cannot make: a 4xx is the message's
        // fault and will fail again identically, a 5xx is the gateway's and might
        // not. Whoever reads this error is a different person in each case.
        match status {
            200..=299 => Ok(message_id(&body, status)),
            400..=499 => Err(SendError::Rejected(format!("{status}: {detail}"))),
            _ => Err(SendError::Unavailable(format!("{status}: {detail}"))),
        }
    }
}

bindings::export!(Component with_types_in bindings);
