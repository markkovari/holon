//! `park-store` — `holon:park/lot` for components, each call one HTTP request
//! to `comp-park` (ADR-0100, ADR-0095).
//!
//! The engine needs a held NATS JetStream connection, which a `wasm32-wasip2`
//! guest cannot dial, so it runs natively (`reconciler/src/bin/park.rs`) and
//! this is its component face: it holds the contract, turns each call into
//! `POST <park-url>/v1/<function>` with the arguments as JSON
//! (`holon_park::wire`), and turns the answer back into the WIT result. It
//! keeps no state and makes no decision.
//!
//! Config (wasi:config/store):
//!   park-url     where `comp-park` listens, e.g. http://127.0.0.1:8015
//!   park-token   the daemon's `--token`, sent as `Authorization: Bearer`, if set
//!
//! Errors: the daemon's refusals arrive as `{error, detail}` and come back as
//! the `park-error` case they name, payload and all. Anything else — no
//! `park-url`, nothing listening, a 401, a body that is not the daemon's JSON —
//! is `storage-error`, which is what the contract says a caller may retry.

#[allow(warnings)]
mod bindings;
mod witconv;

use bindings::exports::holon::park::lot::Guest as Lot;
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};
use bindings::wasi::io::streams::StreamError;

use holon_park::model as m;
use holon_park::wire;
use serde::de::DeserializeOwned;
use serde::Serialize;
use witconv::{ToModel, ToWit};

/// The binding modules `witconv` is compiled against.
mod wit {
    pub use crate::bindings::holon::park::types as t;
}
use wit::t;

struct Component;

/// Five minutes, the same as `vcs-store`'s: a call that is genuinely slow
/// (`comp-park` itself waiting on a CAS retry storm) should not hang a caller
/// forever, but a normal one is milliseconds.
const TIMEOUT_NS: u64 = 300_000_000_000;

fn storage(msg: impl Into<String>) -> t::ParkError {
    t::ParkError::StorageError(msg.into())
}

fn daemon_url() -> Result<String, t::ParkError> {
    match config::get("park-url") {
        Ok(Some(u)) if !u.is_empty() => Ok(u),
        _ => Err(storage("park-url is not set — this component has no comp-park to ask")),
    }
}

fn parse_url(url: &str) -> Result<(Scheme, String, String), t::ParkError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(storage(format!("park-url must be http(s), got {url:?}")));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].trim_end_matches('/').to_string()),
        None => (rest.to_string(), String::new()),
    };
    Ok((scheme, authority, path))
}

/// POST `body` to `/v1/<func>`; the status and the whole response body.
fn post(func: &str, body: &[u8]) -> Result<(u16, Vec<u8>), t::ParkError> {
    let url = daemon_url()?;
    let (scheme, authority, base) = parse_url(&url)?;
    let net = |m: &str| storage(format!("comp-park at {url}: {m}"));

    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    if let Ok(Some(token)) = config::get("park-token") {
        if !token.is_empty() {
            let _ = headers.set("authorization", &[format!("Bearer {token}").into_bytes()]);
        }
    }
    let req = OutgoingRequest::new(headers);
    req.set_method(&Method::Post).map_err(|_| net("set method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net("set scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| net("set authority"))?;
    req.set_path_with_query(Some(&format!("{base}{}", wire::route(func))))
        .map_err(|_| net("set path"))?;

    let out = req.body().map_err(|_| net("body"))?;
    {
        let stream = out.write().map_err(|_| net("write"))?;
        // Chunked: `blocking-write-and-flush` traps above 4096 bytes.
        for chunk in body.chunks(4096) {
            stream
                .blocking_write_and_flush(chunk)
                .map_err(|e| net(&format!("body write: {e:?}")))?;
        }
    }
    OutgoingBody::finish(out, None).map_err(|_| net("finish"))?;

    let opts = RequestOptions::new();
    let _ = opts.set_connect_timeout(Some(TIMEOUT_NS));
    let _ = opts.set_first_byte_timeout(Some(TIMEOUT_NS));
    let _ = opts.set_between_bytes_timeout(Some(TIMEOUT_NS));

    let fut =
        outgoing_handler::handle(req, Some(opts)).map_err(|e| net(&format!("handle: {e:?}")))?;
    fut.subscribe().block();
    let resp = fut
        .get()
        .ok_or_else(|| net("no response"))?
        .map_err(|_| net("response taken"))?
        .map_err(|e| net(&format!("http: {e:?}")))?;
    let status = resp.status();

    let body = resp.consume().map_err(|_| net("consume"))?;
    let stream = body.stream().map_err(|_| net("stream"))?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(c) if c.is_empty() => break,
            Ok(c) => buf.extend_from_slice(&c),
            // `Closed` is end-of-body; anything else is a read that went wrong,
            // and returning what arrived would be a truncated answer.
            Err(StreamError::Closed) => break,
            Err(e) => return Err(net(&format!("read: {e:?}"))),
        }
    }
    Ok((status, buf))
}

/// The daemon's answer, as the WIT result: `200` is the `ok` value; anything
/// else is its `{error, detail}` or, failing that, a storage error.
fn decode<T: DeserializeOwned>(status: u16, body: &[u8]) -> Result<T, t::ParkError> {
    if status == 200 {
        return serde_json::from_slice(body).map_err(|e| {
            storage(format!("comp-park answered 200 with a body this cannot read: {e}"))
        });
    }
    if status == 401 {
        return Err(storage(
            "comp-park refused the credentials (401): park-token must equal its --token",
        ));
    }
    match serde_json::from_slice::<wire::ErrorBody>(body) {
        Ok(e) => Err(e.into_error().to_wit()),
        Err(_) => {
            let text = String::from_utf8_lossy(&body[..body.len().min(200)]).into_owned();
            Err(storage(format!("comp-park answered HTTP {status}: {text}")))
        }
    }
}

/// One call: the request as JSON, the answer as `R` (the model type), then WIT.
fn call<Q: Serialize, R: DeserializeOwned + ToWit>(
    func: &str,
    req: &Q,
) -> Result<R::Out, t::ParkError> {
    let body = serde_json::to_vec(req).map_err(|e| t::ParkError::Invalid(e.to_string()))?;
    let (status, raw) = post(func, &body)?;
    decode::<R>(status, &raw).map(ToWit::to_wit)
}

impl Lot for Component {
    fn park(
        session: String,
        call_: t::OutboundCall,
        by: t::Agent,
    ) -> Result<t::ParkResult, t::ParkError> {
        let req = wire::ParkRequest { session, call: call_.to_model(), by: by.to_model() };
        call::<_, m::ParkResult>("park", &req)
    }

    fn wake(correlation: String, answer: t::CallResult) -> Result<String, t::ParkError> {
        let req = wire::WakeRequest { correlation, answer: answer.to_model() };
        call::<_, String>("wake", &req)
    }

    fn pending(session: String) -> Result<Vec<t::TicketEntry>, t::ParkError> {
        call::<_, Vec<m::TicketEntry>>("pending", &wire::Session { session })
    }

    fn take_ready(ticket: String) -> Result<t::CallResult, t::ParkError> {
        call::<_, m::CallResult>("take-ready", &wire::Ticket { ticket })
    }

    fn cancel(ticket: String, by: t::Agent) -> Result<(), t::ParkError> {
        call::<_, ()>("cancel", &wire::CancelRequest { ticket, by: by.to_model() })
    }

    fn oplog(
        session: String,
        after: Option<u64>,
        limit: u32,
    ) -> Result<Vec<t::TicketEntry>, t::ParkError> {
        call::<_, Vec<m::TicketEntry>>("oplog", &wire::OplogRequest { session, after, limit })
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;
    use holon_park::ParkError;

    fn body(e: &ParkError) -> Vec<u8> {
        serde_json::to_vec(&wire::ErrorBody::from(e)).unwrap()
    }

    /// Every refusal the daemon can send comes back as the case it names, with
    /// its payload — a caller must be able to tell "retry" from "gone" from
    /// "your request was wrong".
    #[test]
    fn daemon_refusals_map_back_to_their_park_error_case() {
        let got = decode::<m::ParkResult>(404, &body(&ParkError::NotFound("t1".into())));
        assert!(matches!(got, Err(t::ParkError::NotFound(s)) if s == "t1"));
        let got = decode::<m::CallResult>(409, &body(&ParkError::AlreadyClosed("t1".into())));
        assert!(matches!(got, Err(t::ParkError::AlreadyClosed(s)) if s == "t1"));
        let got = decode::<m::ParkResult>(400, &body(&ParkError::Invalid("bad".into())));
        assert!(matches!(got, Err(t::ParkError::Invalid(s)) if s == "bad"));
        let got = decode::<m::ParkResult>(503, &body(&ParkError::Storage("down".into())));
        assert!(matches!(got, Err(t::ParkError::StorageError(s)) if s == "down"));
    }

    #[test]
    fn what_is_not_the_daemons_json_is_a_storage_error() {
        assert!(
            matches!(decode::<String>(502, b"<html>bad gateway</html>"), Err(t::ParkError::StorageError(s)) if s.contains("502"))
        );
        assert!(
            matches!(decode::<String>(401, b""), Err(t::ParkError::StorageError(s)) if s.contains("park-token"))
        );
        assert!(matches!(decode::<String>(200, b"not json"), Err(t::ParkError::StorageError(_))));
        assert_eq!(decode::<String>(200, b"\"tkt\"").ok().as_deref(), Some("tkt"));
    }

    #[test]
    fn a_url_with_a_base_path_keeps_it() {
        let (_, auth, base) = parse_url("http://127.0.0.1:8015/").unwrap();
        assert_eq!((auth.as_str(), base.as_str()), ("127.0.0.1:8015", ""));
        let (_, _, base) = parse_url("https://park.internal/api").unwrap();
        assert_eq!(base, "/api");
        assert!(parse_url("nats://x").is_err());
    }

    /// The record with the most shape (an optional `poll`) survives model to
    /// WIT and back.
    #[test]
    fn records_survive_model_to_wit_and_back() {
        let call = m::OutboundCall {
            correlation: "c1".into(),
            description: "d".into(),
            deadline: Some(123),
            poll: Some(m::PollSpec { url: "http://x".into(), interval_secs: 5 }),
        };
        assert_eq!(call.clone().to_wit().to_model(), call);
        let entry = m::TicketEntry {
            ticket: "t".into(),
            session: "s".into(),
            status: m::TurnStatus::Expired,
            parked_at: 1,
            woken_at: None,
            resumed_at: None,
        };
        assert_eq!(entry.clone().to_wit().to_model(), entry);
    }
}
