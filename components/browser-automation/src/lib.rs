//! Neither side trusts the request's url (it can come from a model); egress is the manifest's call (ADR-0008).

#[allow(warnings)]
mod bindings;

use bindings::exports::web::browser::automation::{BrowserError, Guest};
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};
use bindings::wasi::io::streams::StreamError;

struct Component;

const TIMEOUT_NS: u64 = 30_000_000_000;

/// The daemon truncates too, but this side does not trust it: an unbounded read is unbounded guest memory.
const MAX_REPLY_BYTES: usize = 2 * 1024 * 1024;

fn daemon_url() -> Result<String, BrowserError> {
    match config::get("browser-url") {
        Ok(Some(u)) if !u.is_empty() => Ok(u),
        // Unavailable, not not-permitted: nobody refused; the deployment never said where the daemon is.
        _ => Err(BrowserError::Unavailable(
            "browser-url is not set — this component has nowhere to ask".into(),
        )),
    }
}

fn parse_url(url: &str) -> Result<(Scheme, String, String), BrowserError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(BrowserError::Unavailable(format!("browser-url must be http(s), got {url:?}")));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), rest[i..].to_string()),
        None => (rest.to_string(), String::new()),
    };
    Ok((scheme, authority, path))
}

/// The daemon's refusals arrive as JSON with a 200, so any failure here is transport.
fn post(body: Vec<u8>) -> Result<Vec<u8>, BrowserError> {
    let url = daemon_url()?;
    let (scheme, authority, base) = parse_url(&url)?;
    let net = |m: &str| BrowserError::Unavailable(m.to_string());

    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    // Loopback binding alone is not a boundary (see `comp_daemon_kit`); absent means no --token.
    if let Ok(Some(token)) = config::get("browser-token") {
        if !token.is_empty() {
            let _ = headers
                .set(&"authorization".to_string(), &[format!("Bearer {token}").into_bytes()]);
        }
    }
    let req = OutgoingRequest::new(headers);
    req.set_method(&Method::Post).map_err(|_| net("set method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net("set scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| net("set authority"))?;
    req.set_path_with_query(Some(&format!("{base}/snapshot"))).map_err(|_| net("set path"))?;

    let out = req.body().map_err(|_| net("body"))?;
    {
        let stream = out.write().map_err(|_| net("write"))?;
        // `blocking-write-and-flush` traps above 4096 bytes.
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

    let body = resp.consume().map_err(|_| net("consume"))?;
    let stream = body.stream().map_err(|_| net("stream"))?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(c) if c.is_empty() => break,
            Ok(c) => {
                if buf.len() + c.len() > MAX_REPLY_BYTES {
                    return Err(net("reply exceeded the size ceiling"));
                }
                buf.extend_from_slice(&c);
            }
            // Anything but `Closed` is a failed read; returning what arrived would be a truncated answer.
            Err(StreamError::Closed) => break,
            Err(e) => return Err(net(&format!("read: {e:?}"))),
        }
    }
    Ok(buf)
}

/// Two flat reply shapes: serde would be more code than what it reads.
fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = &json[at..];
    let start = rest.find('"')? + 1;
    let end = rest[start..].find('"')? + start;
    Some(&rest[start..end])
}

impl Guest for Component {
    fn snapshot(url: String) -> Result<String, BrowserError> {
        let body = format!("{{\"url\":{}}}", json_str(&url));
        let raw = post(body.into_bytes())?;
        let text = String::from_utf8_lossy(&raw).into_owned();

        // Mapped back to the variant, or a caller retries a permanent "no" forever.
        if let Some(err) = field(&text, "error") {
            let detail = field(&text, "detail").unwrap_or_default().to_string();
            return Err(match err {
                "not-permitted" => BrowserError::NotPermitted(detail),
                other => BrowserError::Unavailable(format!("{other}: {detail}")),
            });
        }

        match field(&text, "content") {
            Some(content) => Ok(content.to_string()),
            None => Err(BrowserError::Unavailable("daemon reply had no content field".into())),
        }
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_cannot_break_out_of_the_request_it_is_put_in() {
        assert_eq!(json_str("https://a/b"), r#""https://a/b""#);
        assert_eq!(json_str(r#"https://a"b"#), r#""https://a\"b""#);
        assert_eq!(json_str(r#"https://a\b"#), r#""https://a\\b""#);
        // Escaped, not stripped: removing it would silently change the url.
        assert_eq!(json_str("https://a\u{1}b"), "\"https://a\\u0001b\"");
    }

    #[test]
    fn a_successful_reply_yields_its_content() {
        let json = r#"{"content":"<html>hi</html>"}"#;
        assert_eq!(field(json, "content"), Some("<html>hi</html>"));
        assert!(field(json, "error").is_none());
    }

    #[test]
    fn a_refusal_is_read_back_as_not_permitted() {
        let json = r#"{"error":"not-permitted","detail":"evil.example"}"#;
        assert_eq!(field(json, "error"), Some("not-permitted"));
        assert_eq!(field(json, "detail"), Some("evil.example"));
    }
}
