//! `notify-dispatch` — reference implementation of `notify:dispatch`.
//!
//! Synchronous outbound sender over `wasi:http/outgoing-handler`. The HTTP idiom
//! (build OutgoingRequest -> write body -> handle -> block -> read response) is
//! the same one auth-guard uses for OIDC; factored here into `post`.
//!
//! Channels:
//!   webhook  POST `body` to `target` as application/json.
//!   email    POST {"to","subject","body"} to config `notify:email-url`.
//!   sms      POST {"to","body"} to config `notify:sms-url`.
//! The gateway URLs are deploy-time config; no vendor is named in the contract.

#[allow(warnings)]
mod bindings;

use bindings::exports::notify::dispatch::dispatcher::{Channel, Guest, Message, NotifyError};
use bindings::wasi::config::runtime as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};
use bindings::wasi::io::streams::StreamError;

struct Component;

fn net_err(ctx: &str) -> NotifyError {
    NotifyError::BackendUnavailable(ctx.to_string())
}

/// JSON-escape a string value.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn parse_url(url: &str) -> Result<(Scheme, String, String), NotifyError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(NotifyError::BackendUnavailable(format!("bad url scheme: {url}")));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].to_string()),
        None => (rest.to_string(), "/".to_string()),
    };
    Ok((scheme, authority, path))
}

/// POST `body` (application/json) to `url`, returning the upstream status.
fn post(url: &str, body: &[u8]) -> Result<u16, NotifyError> {
    let (scheme, authority, path) = parse_url(url)?;
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);

    let req = OutgoingRequest::new(headers);
    req.set_method(&Method::Post).map_err(|_| net_err("set method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net_err("set scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| net_err("set authority"))?;
    req.set_path_with_query(Some(&path)).map_err(|_| net_err("set path"))?;

    {
        let out = req.body().map_err(|_| net_err("body"))?;
        {
            let stream = out.write().map_err(|_| net_err("write stream"))?;
            stream
                .blocking_write_and_flush(body)
                .map_err(|e| net_err(&format!("body write: {e:?}")))?;
        }
        OutgoingBody::finish(out, None).map_err(|_| net_err("finish body"))?;
    }

    let future = outgoing_handler::handle(req, Some(RequestOptions::new()))
        .map_err(|e| NotifyError::BackendUnavailable(format!("http handle: {e:?}")))?;
    future.subscribe().block();
    let resp = future
        .get()
        .ok_or_else(|| net_err("no response"))?
        .map_err(|_| net_err("response taken"))?
        .map_err(|e| NotifyError::BackendUnavailable(format!("http: {e:?}")))?;

    let status = resp.status();
    // Drain the body so the connection is released.
    if let Ok(incoming) = resp.consume() {
        if let Ok(stream) = incoming.stream() {
            loop {
                match stream.blocking_read(8192) {
                    Ok(c) if c.is_empty() => break,
                    Ok(_) => {}
                    Err(StreamError::Closed) => break,
                    Err(_) => break,
                }
            }
        }
    }

    if (200..300).contains(&status) {
        Ok(status)
    } else {
        Err(NotifyError::DeliveryFailed(format!("upstream status {status}")))
    }
}

fn gateway(key: &str) -> Option<String> {
    config::get(key).ok().flatten().filter(|s| !s.is_empty())
}

impl Guest for Component {
    fn send(msg: Message) -> Result<u16, NotifyError> {
        match msg.channel {
            Channel::Webhook => post(&msg.target, msg.body.as_bytes()),
            Channel::Email => {
                let url = gateway("notify:email-url")
                    .ok_or_else(|| NotifyError::UnsupportedChannel("email: no notify:email-url".into()))?;
                let payload = format!(
                    "{{\"to\":\"{}\",\"subject\":\"{}\",\"body\":\"{}\"}}",
                    esc(&msg.target),
                    esc(&msg.subject),
                    esc(&msg.body),
                );
                post(&url, payload.as_bytes())
            }
            Channel::Sms => {
                let url = gateway("notify:sms-url")
                    .ok_or_else(|| NotifyError::UnsupportedChannel("sms: no notify:sms-url".into()))?;
                let payload =
                    format!("{{\"to\":\"{}\",\"body\":\"{}\"}}", esc(&msg.target), esc(&msg.body));
                post(&url, payload.as_bytes())
            }
        }
    }
}

bindings::export!(Component with_types_in bindings);
