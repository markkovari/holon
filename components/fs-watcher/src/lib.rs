//! `fs-watcher` — report what changed in a directory, since a cursor
//!
//! The watching happens in `comp-fswatch`, which is native because a watch
//! syscall is not something a `wasm32-wasip2` guest has (ADR-0095). This is the
//! component side: it holds the contract, and reaches the daemon over HTTP the
//! same way `checks-runner` reaches `comp-checks`.
//!
//! Two things follow from that split, and both are the point rather than a cost:
//!
//!   * what this may dial is a MANIFEST decision (ADR-0008). The daemon's
//!     address is `fswatch-url` in `wasi:config`, and the deployment's egress
//!     allow-list decides whether the call leaves at all. A component that could
//!     reach any address would make the allow-list decorative.
//!   * the daemon has its own allow-list of directories. Neither side trusts the
//!     path in the request, because it can come from a model.
//!
//! Config (wasi:config/store):
//!   fswatch-url    where `comp-fswatch` is listening, e.g. http://127.0.0.1:8car
//!
//! It used to return `format!("Watching {} for changes...", dir)` — a sentence
//! shaped like an answer, which no caller could tell from a real one. That is
//! what the old `-> string` contract permitted; the current one cannot express
//! it.

#[allow(warnings)]
mod bindings;

use bindings::exports::os::fs::watcher::{Change, Changes, Event, Guest, WatchError};
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme};
use bindings::wasi::io::streams::StreamError;

struct Component;

/// Ten seconds. A poll stats a directory and returns; anything slower than this
/// is a daemon in trouble rather than a big directory, and a caller waiting on
/// an HTTP request would rather hear that than wait.
const TIMEOUT_NS: u64 = 10_000_000_000;

fn daemon_url() -> Result<String, WatchError> {
    match config::get("fswatch-url") {
        Ok(Some(u)) if !u.is_empty() => Ok(u),
        // Unavailable rather than not-permitted: nobody refused anything, the
        // deployment simply never said where the watcher is.
        _ => Err(WatchError::Unavailable(
            "fswatch-url is not set — this watcher has nowhere to ask".into(),
        )),
    }
}

fn parse_url(url: &str) -> Result<(Scheme, String, String), WatchError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(WatchError::Unavailable(format!("fswatch-url must be http(s), got {url:?}")));
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
fn post(body: Vec<u8>) -> Result<Vec<u8>, WatchError> {
    let url = daemon_url()?;
    let (scheme, authority, base) = parse_url(&url)?;
    let net = |m: &str| WatchError::Unavailable(m.to_string());

    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    let req = OutgoingRequest::new(headers);
    req.set_method(&Method::Post).map_err(|_| net("set method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net("set scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| net("set authority"))?;
    req.set_path_with_query(Some(&format!("{base}/poll"))).map_err(|_| net("set path"))?;

    let out = req.body().map_err(|_| net("body"))?;
    {
        let stream = out.write().map_err(|_| net("write"))?;
        // Chunked: `blocking-write-and-flush` traps above 4096 bytes, and a
        // request naming a long path plus a cursor is small but not bounded.
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
/// The daemon's replies have four shapes and no nesting beyond a flat event
/// list, so a full serde dependency would be more code than the thing it reads.
fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = &json[at..];
    let start = rest.find('"')? + 1;
    let end = rest[start..].find('"')? + start;
    Some(&rest[start..end])
}

fn events_of(json: &str) -> Vec<Event> {
    let mut out = Vec::new();
    let Some(list_at) = json.find("\"events\"") else { return out };
    for chunk in json[list_at..].split("{\"path\":").skip(1) {
        let obj = format!("{{\"path\":{chunk}");
        let Some(path) = field(&obj, "path") else { continue };
        let kind = match field(&obj, "kind") {
            Some("created") => Change::Created,
            Some("removed") => Change::Removed,
            _ => Change::Modified,
        };
        let at = obj
            .find("\"at\":")
            .map(|i| &obj[i + 5..])
            .and_then(|r| r.trim_start().split([',', '}']).next())
            .and_then(|n| n.trim().parse::<u64>().ok())
            .unwrap_or(0);
        out.push(Event { path: path.to_string(), kind, at });
    }
    out
}

impl Guest for Component {
    fn poll(dir: String, cursor: String) -> Result<Changes, WatchError> {
        let body = format!(
            "{{\"dir\":{},\"cursor\":{}}}",
            json_str(&dir),
            json_str(&cursor)
        );
        let raw = post(body.into_bytes())?;
        let text = String::from_utf8_lossy(&raw).into_owned();

        // The daemon reports its refusals in the body. Mapping them back to the
        // variant matters: a caller that cannot tell "you may not watch that"
        // from "the watcher is down" will retry the first one forever.
        if let Some(err) = field(&text, "error") {
            let detail = field(&text, "detail").unwrap_or_default().to_string();
            return Err(match err {
                "not-permitted" => WatchError::NotPermitted(detail),
                "no-such-directory" => WatchError::NoSuchDirectory(detail),
                other => WatchError::Unavailable(format!("{other}: {detail}")),
            });
        }

        Ok(Changes {
            events: events_of(&text),
            cursor: field(&text, "cursor").unwrap_or_default().to_string(),
            truncated: text.contains("\"truncated\":true"),
        })
    }
}

/// A JSON string literal. A path can contain a quote or a backslash, and this is
/// building a request out of one.
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

    /// A path is caller-supplied and goes into a JSON request. A quote in it
    /// would end the string early and the rest would be read as structure.
    #[test]
    fn a_path_cannot_break_out_of_the_request_it_is_put_in() {
        assert_eq!(json_str(r#"/a/b"#), r#""/a/b""#);
        assert_eq!(json_str(r#"/a"b"#), r#""/a\"b""#);
        assert_eq!(json_str(r#"/a\b"#), r#""/a\\b""#);
        assert_eq!(json_str("/a\nb"), r#""/a\nb""#);
        // Escaped, not stripped: a control character silently removed would
        // change the path the daemon is asked about, and nobody would know.
        assert_eq!(json_str("/a\u{1}b"), r#""/a\u0001b""#);
    }

    #[test]
    fn events_are_read_back_with_their_kind_and_time() {
        let json = r#"{"events":[{"path":"/w/a","kind":"created","at":17},
                                 {"path":"/w/b","kind":"removed","at":18}],
                       "cursor":"18","truncated":false}"#;
        let ev = events_of(json);
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].path, "/w/a");
        assert!(matches!(ev[0].kind, Change::Created));
        assert_eq!(ev[0].at, 17);
        assert!(matches!(ev[1].kind, Change::Removed));
        assert_eq!(field(json, "cursor"), Some("18"));
    }

    /// An empty page is a legitimate answer — the first poll always returns one
    /// — and must not read as a failure.
    #[test]
    fn an_empty_page_is_not_an_error() {
        let json = r#"{"events":[],"cursor":"99","truncated":false}"#;
        assert!(events_of(json).is_empty());
        assert_eq!(field(json, "cursor"), Some("99"));
        assert!(field(json, "error").is_none());
    }
}
