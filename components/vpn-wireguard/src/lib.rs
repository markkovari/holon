//! `vpn-wireguard` — whether a WireGuard tunnel is up and which peers are connected
//!
//! The inspection happens in `comp-wireguard`, which is native because reading
//! a WireGuard interface needs the `wg` binary and a kernel module, neither of
//! which a `wasm32-wasip2` guest has (ADR-0095). This is the component side: it
//! holds the contract, and reaches the daemon over HTTP the same way
//! `fs-watcher` reaches `comp-fswatch`.
//!
//! Two things follow from that split, and both are the point rather than a cost:
//!
//!   * what this may dial is a MANIFEST decision (ADR-0008). The daemon's
//!     address is `wireguard-url` in `wasi:config`, and the deployment's
//!     egress allow-list decides whether the call leaves at all.
//!   * the daemon has its own allow-list of interfaces, set by its operator at
//!     startup. `status` takes no argument, so there is nothing here for a
//!     caller to name that could be refused.
//!
//! Config (wasi:config/store):
//!   wireguard-url    where `comp-wireguard` is listening, e.g. http://127.0.0.1:8011
//!
//! It used to return `"UNIMPLEMENTED: ..."` — a sentence a caller could not
//! tell apart from a real answer. That is what the old `-> string` contract
//! permitted; the current one cannot express it.

#[allow(warnings)]
mod bindings;

use bindings::exports::net::vpn::wireguard::{Guest, Peer, WgError};
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme};
use bindings::wasi::io::streams::StreamError;

struct Component;

/// Ten seconds. `wg show ... dump` returns immediately; anything slower than
/// this is a daemon in trouble.
const TIMEOUT_NS: u64 = 10_000_000_000;

fn daemon_url() -> Result<String, WgError> {
    match config::get("wireguard-url") {
        Ok(Some(u)) if !u.is_empty() => Ok(u),
        // Unavailable rather than not-permitted: nobody refused anything, the
        // deployment simply never said where the daemon is.
        _ => Err(WgError::Unavailable(
            "wireguard-url is not set — this component has nowhere to ask".into(),
        )),
    }
}

fn parse_url(url: &str) -> Result<(Scheme, String, String), WgError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(WgError::Unavailable(format!("wireguard-url must be http(s), got {url:?}")));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].to_string()),
        None => (rest.to_string(), String::new()),
    };
    Ok((scheme, authority, path))
}

/// POST the request and return the body. Every failure is `unavailable`: the
/// daemon's own refusals arrive as JSON with a 200, so anything at this level is
/// the transport rather than an answer.
fn post(body: Vec<u8>) -> Result<Vec<u8>, WgError> {
    let url = daemon_url()?;
    let (scheme, authority, base) = parse_url(&url)?;
    let net = |m: &str| WgError::Unavailable(m.to_string());

    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    let req = OutgoingRequest::new(headers);
    req.set_method(&Method::Post).map_err(|_| net("set method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net("set scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| net("set authority"))?;
    req.set_path_with_query(Some(&format!("{base}/status"))).map_err(|_| net("set path"))?;

    let out = req.body().map_err(|_| net("body"))?;
    {
        let stream = out.write().map_err(|_| net("write"))?;
        // Chunked: `blocking-write-and-flush` traps above 4096 bytes.
        for chunk in body.chunks(4096) {
            stream.blocking_write_and_flush(chunk).map_err(|e| net(&format!("body write: {e:?}")))?;
        }
    }
    OutgoingBody::finish(out, None).map_err(|_| net("finish"))?;

    let opts = RequestOptions::new();
    let _ = opts.set_connect_timeout(Some(TIMEOUT_NS));
    let _ = opts.set_first_byte_timeout(Some(TIMEOUT_NS));
    let _ = opts.set_between_bytes_timeout(Some(TIMEOUT_NS));

    let fut = outgoing_handler::handle(req, Some(opts)).map_err(|e| net(&format!("handle: {e:?}")))?;
    fut.subscribe().block();
    let resp = fut
        .get()
        .ok_or_else(|| net("no response"))?
        .map_err(|_| net("response taken"))?
        .map_err(|e| net(&format!("http: {e:?}")))?;

    let body = resp.consume().map_err(|_| net("consume"))?;
    let stream = body.stream().map_err(|_| net("stream"))?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(c) if c.is_empty() => break,
            Ok(c) => buf.extend_from_slice(&c),
            // `Closed` is end-of-body; anything else is a read that went wrong,
            // and returning what arrived would be a truncated answer presented
            // as a whole one.
            Err(StreamError::Closed) => break,
            Err(e) => return Err(net(&format!("read: {e:?}"))),
        }
    }
    Ok(buf)
}

/// Pull one JSON string field out without a parser.
///
/// The daemon's replies have two shapes and no nesting beyond a flat peer
/// list, so a full serde dependency would be more code than the thing it reads.
fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = &json[at..];
    let start = rest.find('"')? + 1;
    let end = rest[start..].find('"')? + start;
    Some(&rest[start..end])
}

/// Pull one JSON numeric field out.
fn number(json: &str, key: &str) -> u64 {
    json.find(&format!("\"{key}\":"))
        .map(|i| &json[i + key.len() + 3..])
        .and_then(|r| r.trim_start().split([',', '}']).next())
        .and_then(|n| n.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

fn peers_of(json: &str) -> Vec<Peer> {
    let mut out = Vec::new();
    let Some(list_at) = json.find("\"peers\"") else { return out };
    for chunk in json[list_at..].split("{\"public_key\":").skip(1) {
        let obj = format!("{{\"public_key\":{chunk}");
        let Some(public_key) = field(&obj, "public_key") else { continue };
        let endpoint = field(&obj, "endpoint").unwrap_or_default().to_string();
        out.push(Peer {
            public_key: public_key.to_string(),
            endpoint,
            latest_handshake: number(&obj, "latest_handshake"),
            rx_bytes: number(&obj, "rx_bytes"),
            tx_bytes: number(&obj, "tx_bytes"),
        });
    }
    out
}

impl Guest for Component {
    fn status() -> Result<Vec<Peer>, WgError> {
        let raw = post(b"{}".to_vec())?;
        let text = String::from_utf8_lossy(&raw).into_owned();

        // The daemon reports its refusals in the body. `not-permitted` is
        // never actually sent by `comp-wireguard` — this contract takes no
        // argument for it to refuse — but is mapped here anyway, for the same
        // reason it exists in the WIT: symmetry with the other capabilities'
        // allow-list-refusal shape.
        if let Some(err) = field(&text, "error") {
            let detail = field(&text, "detail").unwrap_or_default().to_string();
            return Err(match err {
                "not-permitted" => WgError::NotPermitted(detail),
                other => WgError::Unavailable(format!("{other}: {detail}")),
            });
        }

        Ok(peers_of(&text))
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peers_are_read_back_with_their_counters() {
        let json = r#"{"peers":[{"public_key":"abc123=","endpoint":"1.2.3.4:51820",
                                 "latest_handshake":1700000000,"rx_bytes":1024,"tx_bytes":2048}]}"#;
        let peers = peers_of(json);
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].public_key, "abc123=");
        assert_eq!(peers[0].endpoint, "1.2.3.4:51820");
        assert_eq!(peers[0].latest_handshake, 1700000000);
        assert_eq!(peers[0].rx_bytes, 1024);
        assert_eq!(peers[0].tx_bytes, 2048);
    }

    /// No allowed interface (or none with peers) is a legitimate answer, not a
    /// failure — same reasoning as an unscoped fs-watcher watching nothing.
    #[test]
    fn an_empty_peer_list_is_not_an_error() {
        let json = r#"{"peers":[]}"#;
        assert!(peers_of(json).is_empty());
        assert!(field(json, "error").is_none());
    }

    #[test]
    fn an_error_body_carries_a_detail() {
        let json = r#"{"error":"unavailable","detail":"wg binary not found"}"#;
        assert_eq!(field(json, "error"), Some("unavailable"));
        assert_eq!(field(json, "detail"), Some("wg binary not found"));
    }
}
