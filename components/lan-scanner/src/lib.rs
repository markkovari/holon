//! `lan-scanner` — which hosts on the local network answer, for a component that cannot look
//!
//! The scanning happens in `comp-lanscan`, which is native because opening
//! many outbound TCP connections against addresses a caller does not supply
//! is not something a `wasm32-wasip2` guest can do (ADR-0095). This is the
//! component side: it holds the contract, and reaches the daemon over HTTP the
//! same way `fs-watcher` reaches `comp-fswatch`.
//!
//! Two things follow from that split, and both are the point rather than a cost:
//!
//!   * what this may dial is a MANIFEST decision (ADR-0008). The daemon's
//!     address is `lanscan-url` in `wasi:config`, and the deployment's egress
//!     allow-list decides whether the call leaves at all.
//!   * the daemon has its own allow-list of CIDRs. There is no target in the
//!     request at all — the scope of a scan is a daemon-side decision, the
//!     same way fs-watcher's directories are daemon-side rather than named by
//!     the caller.
//!
//! Config (wasi:config/store):
//!   lanscan-url    where `comp-lanscan` is listening, e.g. http://127.0.0.1:8005
//!
//! It used to return `"UNIMPLEMENTED: ..."` — a sentence a caller could not
//! tell apart from a real answer. That is what the old `-> string` contract
//! permitted; the current one cannot express it.

#[allow(warnings)]
mod bindings;

use bindings::exports::net::lan::scanner::{Guest, Host, ScanError};
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme};
use bindings::wasi::io::streams::StreamError;

struct Component;

/// Ten seconds. A scan of at most 254 hosts on the daemon side is bounded;
/// anything slower than this is a daemon in trouble rather than a big LAN.
const TIMEOUT_NS: u64 = 10_000_000_000;

fn daemon_url() -> Result<String, ScanError> {
    match config::get("lanscan-url") {
        Ok(Some(u)) if !u.is_empty() => Ok(u),
        // Unavailable rather than not-permitted: nobody refused anything, the
        // deployment simply never said where the scanner is.
        _ => Err(ScanError::Unavailable(
            "lanscan-url is not set — this scanner has nowhere to ask".into(),
        )),
    }
}

fn parse_url(url: &str) -> Result<(Scheme, String, String), ScanError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(ScanError::Unavailable(format!("lanscan-url must be http(s), got {url:?}")));
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
fn post(body: Vec<u8>) -> Result<Vec<u8>, ScanError> {
    let url = daemon_url()?;
    let (scheme, authority, base) = parse_url(&url)?;
    let net = |m: &str| ScanError::Unavailable(m.to_string());

    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    // A shared secret, if the deployment set one — see
    // `comp_reconciler::daemon_auth`'s own doc for why loopback
    // binding alone is not a boundary. Absent means the daemon was
    // started with no --token, so there is nothing to send.
    if let Ok(Some(token)) = config::get("lanscan-token") {
        if !token.is_empty() {
            let _ = headers.set(&"authorization".to_string(), &[format!("Bearer {token}").into_bytes()]);
        }
    }
    let req = OutgoingRequest::new(headers);
    req.set_method(&Method::Post).map_err(|_| net("set method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net("set scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| net("set authority"))?;
    req.set_path_with_query(Some(&format!("{base}/scan"))).map_err(|_| net("set path"))?;

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
/// The daemon's replies have two shapes and no nesting beyond a flat host
/// list, so a full serde dependency would be more code than the thing it reads.
fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = &json[at..];
    let start = rest.find('"')? + 1;
    let end = rest[start..].find('"')? + start;
    Some(&rest[start..end])
}

fn hosts_of(json: &str) -> Vec<Host> {
    let mut out = Vec::new();
    let Some(list_at) = json.find("\"hosts\"") else { return out };
    for chunk in json[list_at..].split("{\"ip\":").skip(1) {
        let obj = format!("{{\"ip\":{chunk}");
        let Some(ip) = field(&obj, "ip") else { continue };
        let reachable = obj
            .find("\"reachable\":")
            .map(|i| &obj[i + 12..])
            .map(|r| r.trim_start().starts_with("true"))
            .unwrap_or(false);
        out.push(Host { ip: ip.to_string(), reachable });
    }
    out
}

impl Guest for Component {
    fn scan() -> Result<Vec<Host>, ScanError> {
        let raw = post(b"{}".to_vec())?;
        let text = String::from_utf8_lossy(&raw).into_owned();

        // The daemon reports its refusals in the body. Mapping them back to
        // the variant matters even though this contract only has one error
        // case: a body that failed to parse should not silently read as an
        // empty, successful scan.
        if let Some(err) = field(&text, "error") {
            let detail = field(&text, "detail").unwrap_or_default().to_string();
            return Err(ScanError::Unavailable(format!("{err}: {detail}")));
        }

        Ok(hosts_of(&text))
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_are_read_back_with_reachability() {
        let json = r#"{"hosts":[{"ip":"192.168.1.1","reachable":true},
                                {"ip":"192.168.1.2","reachable":false}]}"#;
        let hosts = hosts_of(json);
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].ip, "192.168.1.1");
        assert!(hosts[0].reachable);
        assert_eq!(hosts[1].ip, "192.168.1.2");
        assert!(!hosts[1].reachable);
    }

    /// An empty scan is a legitimate answer — every allowed host was
    /// unreachable — and must not read as a failure.
    #[test]
    fn an_empty_scan_is_not_an_error() {
        let json = r#"{"hosts":[]}"#;
        assert!(hosts_of(json).is_empty());
        assert!(field(json, "error").is_none());
    }

    #[test]
    fn an_error_body_carries_a_detail() {
        let json = r#"{"error":"unavailable","detail":"no --allow-cidr configured"}"#;
        assert_eq!(field(json, "error"), Some("unavailable"));
        assert_eq!(field(json, "detail"), Some("no --allow-cidr configured"));
    }
}
