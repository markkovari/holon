//! `mdns-discovery` — what a printer, speaker or other device is announcing
//! on the local network
//!
//! Browsing mDNS/DNS-SD needs multicast on the host network, which a
//! `wasm32-wasip2` guest does not have (ADR-0095). This is the component
//! side: it holds the contract, and reaches the daemon over HTTP the same way
//! `fs-watcher` reaches `comp-fswatch`.
//!
//! What this may dial is a MANIFEST decision (ADR-0008): the daemon's address
//! is `mdns-url` in `wasi:config`, and the deployment's egress allow-list
//! decides whether the call leaves at all.
//!
//! Config (wasi:config/store):
//!   mdns-url    where `comp-mdns` is listening, e.g. http://127.0.0.1:8007

#[allow(warnings)]
mod bindings;

use bindings::exports::net::mdns::discovery::{Guest, MdnsError, Service};
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme};
use bindings::wasi::io::streams::StreamError;

struct Component;

/// Ten seconds, same reasoning as `fs-watcher`: a browse is a bounded local
/// operation, and anything slower is a daemon in trouble rather than a slow
/// network.
const TIMEOUT_NS: u64 = 10_000_000_000;

fn daemon_url() -> Result<String, MdnsError> {
    match config::get("mdns-url") {
        Ok(Some(u)) if !u.is_empty() => Ok(u),
        // Unavailable rather than not-permitted: nobody refused anything, the
        // deployment simply never said where the browser is.
        _ => Err(MdnsError::Unavailable(
            "mdns-url is not set — this browser has nowhere to ask".into(),
        )),
    }
}

fn parse_url(url: &str) -> Result<(Scheme, String, String), MdnsError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(MdnsError::Unavailable(format!("mdns-url must be http(s), got {url:?}")));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].to_string()),
        None => (rest.to_string(), String::new()),
    };
    Ok((scheme, authority, path))
}

/// POST the request and return the body. Every failure is `unavailable`: the
/// daemon's own refusals arrive as JSON with a 200, so anything at this level
/// is the transport rather than an answer.
fn post(body: Vec<u8>) -> Result<Vec<u8>, MdnsError> {
    let url = daemon_url()?;
    let (scheme, authority, base) = parse_url(&url)?;
    let net = |m: &str| MdnsError::Unavailable(m.to_string());

    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    let req = OutgoingRequest::new(headers);
    req.set_method(&Method::Post).map_err(|_| net("set method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net("set scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| net("set authority"))?;
    req.set_path_with_query(Some(&format!("{base}/discover"))).map_err(|_| net("set path"))?;

    let out = req.body().map_err(|_| net("body"))?;
    {
        let stream = out.write().map_err(|_| net("write"))?;
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
            // `Closed` is end-of-body; anything else is a read that went
            // wrong, and returning what arrived would be a truncated answer
            // presented as a whole one.
            Err(StreamError::Closed) => break,
            Err(e) => return Err(net(&format!("read: {e:?}"))),
        }
    }
    Ok(buf)
}

/// Pull one JSON string field out without a parser.
///
/// The daemon's replies have two shapes and no nesting beyond a flat service
/// list, so a full serde dependency would be more code than the thing it reads.
fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = &json[at..];
    let start = rest.find('"')? + 1;
    let end = rest[start..].find('"')? + start;
    Some(&rest[start..end])
}

fn services_of(json: &str) -> Vec<Service> {
    let mut out = Vec::new();
    let Some(list_at) = json.find("\"services\"") else { return out };
    for chunk in json[list_at..].split("{\"name\":").skip(1) {
        let obj = format!("{{\"name\":{chunk}");
        let Some(name) = field(&obj, "name") else { continue };
        let Some(host) = field(&obj, "host") else { continue };
        let port = obj
            .find("\"port\":")
            .map(|i| &obj[i + 7..])
            .and_then(|r| r.trim_start().split([',', '}']).next())
            .and_then(|n| n.trim().parse::<u16>().ok())
            .unwrap_or(0);
        out.push(Service { name: name.to_string(), host: host.to_string(), port });
    }
    out
}

impl Guest for Component {
    fn discover() -> Result<Vec<Service>, MdnsError> {
        let raw = post(b"{}".to_vec())?;
        let text = String::from_utf8_lossy(&raw).into_owned();

        if let Some(err) = field(&text, "error") {
            let detail = field(&text, "detail").unwrap_or_default().to_string();
            return Err(MdnsError::Unavailable(format!("{err}: {detail}")));
        }

        Ok(services_of(&text))
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn services_are_read_back_with_host_and_port() {
        let json = r#"{"services":[{"name":"Kitchen Printer._ipp._tcp.local.","host":"kitchen.local","port":631},
                                    {"name":"Speaker._raop._tcp.local.","host":"speaker.local","port":5000}]}"#;
        let s = services_of(json);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, "Kitchen Printer._ipp._tcp.local.");
        assert_eq!(s[0].host, "kitchen.local");
        assert_eq!(s[0].port, 631);
        assert_eq!(s[1].port, 5000);
    }

    /// Nothing announcing itself is a real, successful answer — not an error.
    #[test]
    fn an_empty_network_is_not_an_error() {
        let json = r#"{"services":[]}"#;
        assert!(services_of(json).is_empty());
        assert!(field(json, "error").is_none());
    }

    #[test]
    fn an_unavailable_daemon_is_reported_as_such() {
        let json = r#"{"error":"unavailable","detail":"daemon not configured"}"#;
        assert_eq!(field(json, "error"), Some("unavailable"));
        assert_eq!(field(json, "detail"), Some("daemon not configured"));
    }
}
