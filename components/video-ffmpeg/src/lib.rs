//! `video-ffmpeg` — re-encode a video into another format, for a component
//! that cannot spawn a process
//!
//! The transcode happens in `comp-ffmpeg`, which is native because ffmpeg is a
//! process and a `wasm32-wasip2` guest cannot fork one (ADR-0095). This is the
//! component side: it holds the contract, and reaches the daemon over HTTP the
//! same way `fs-watcher` reaches `comp-fswatch`.
//!
//! Two things follow from that split, and both are the point rather than a cost:
//!
//!   * what this may dial is a MANIFEST decision (ADR-0008). The daemon's
//!     address is `ffmpeg-url` in `wasi:config`, and the deployment's egress
//!     allow-list decides whether the call leaves at all.
//!   * the daemon has its own allow-list of directories. Neither side trusts
//!     the path in the request, because it can come from a model.
//!
//! Config (wasi:config/store):
//!   ffmpeg-url    where `comp-ffmpeg` is listening, e.g. http://127.0.0.1:8010
//!
//! It used to return `format!("UNIMPLEMENTED: ...")` — a sentence shaped like
//! an answer, which no caller could tell from a real one. That is what the old
//! `-> string` contract permitted; the current one cannot express it.

#[allow(warnings)]
mod bindings;

use bindings::exports::media::video::ffmpeg::{Guest, TranscodeError};
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme};
use bindings::wasi::io::streams::StreamError;

struct Component;

/// Sixty seconds. A transcode is a real ffmpeg run and can take a while even
/// for a short clip; a caller waiting on an HTTP request would rather hear
/// "unavailable" than wait forever, but ten seconds (fs-watcher's poll budget)
/// would time out on anything but a trivial file.
const TIMEOUT_NS: u64 = 60_000_000_000;

fn daemon_url() -> Result<String, TranscodeError> {
    match config::get("ffmpeg-url") {
        Ok(Some(u)) if !u.is_empty() => Ok(u),
        // Unavailable rather than not-permitted: nobody refused anything, the
        // deployment simply never said where the transcoder is.
        _ => Err(TranscodeError::Unavailable(
            "ffmpeg-url is not set — this transcoder has nowhere to ask".into(),
        )),
    }
}

fn parse_url(url: &str) -> Result<(Scheme, String, String), TranscodeError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(TranscodeError::Unavailable(format!("ffmpeg-url must be http(s), got {url:?}")));
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
fn post(body: Vec<u8>) -> Result<Vec<u8>, TranscodeError> {
    let url = daemon_url()?;
    let (scheme, authority, base) = parse_url(&url)?;
    let net = |m: &str| TranscodeError::Unavailable(m.to_string());

    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    let req = OutgoingRequest::new(headers);
    req.set_method(&Method::Post).map_err(|_| net("set method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net("set scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| net("set authority"))?;
    req.set_path_with_query(Some(&format!("{base}/transcode"))).map_err(|_| net("set path"))?;

    let out = req.body().map_err(|_| net("body"))?;
    {
        let stream = out.write().map_err(|_| net("write"))?;
        // Chunked: `blocking-write-and-flush` traps above 4096 bytes, and a
        // request naming a long path is small but not bounded.
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
/// The daemon's replies have two flat shapes, so a full serde dependency
/// would be more code than the thing it reads.
fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = &json[at..];
    let start = rest.find('"')? + 1;
    let end = rest[start..].find('"')? + start;
    Some(&rest[start..end])
}

impl Guest for Component {
    fn transcode(input: String) -> Result<String, TranscodeError> {
        let body = format!("{{\"input\":{}}}", json_str(&input));
        let raw = post(body.into_bytes())?;
        let text = String::from_utf8_lossy(&raw).into_owned();

        // The daemon reports its refusals in the body. Mapping them back to
        // the variant matters: a caller that cannot tell "you may not
        // transcode that" from "the transcoder is down" will retry the first
        // one forever.
        if let Some(err) = field(&text, "error") {
            let detail = field(&text, "detail").unwrap_or_default().to_string();
            return Err(match err {
                "not-permitted" => TranscodeError::NotPermitted(detail),
                "no-such-file" => TranscodeError::NoSuchFile(detail),
                other => TranscodeError::Unavailable(format!("{other}: {detail}")),
            });
        }

        match field(&text, "output") {
            Some(output) => Ok(output.to_string()),
            None => Err(TranscodeError::Unavailable(format!("no output in reply: {text}"))),
        }
    }
}

/// A JSON string literal. A path can contain a quote or a backslash, and this
/// is building a request out of one.
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
        assert_eq!(json_str(r#"/a/b.mov"#), r#""/a/b.mov""#);
        assert_eq!(json_str(r#"/a"b"#), r#""/a\"b""#);
        assert_eq!(json_str(r#"/a\b"#), r#""/a\\b""#);
        assert_eq!(json_str("/a\nb"), r#""/a\nb""#);
        // Escaped, not stripped: a control character silently removed would
        // change the path the daemon is asked about, and nobody would know.
        assert_eq!(json_str("/a\u{1}b"), r#""/a\u0001b""#);
    }

    #[test]
    fn a_success_reply_is_read_back_as_the_output_path() {
        let json = r#"{"output":"/a/b.mp4"}"#;
        assert_eq!(field(json, "output"), Some("/a/b.mp4"));
        assert!(field(json, "error").is_none());
    }

    /// The daemon's three refusal shapes must map back to the matching
    /// variant, not all collapse to `unavailable` — a caller retrying a
    /// permission refusal forever is the failure mode this guards against.
    #[test]
    fn a_refusal_names_its_own_kind() {
        let not_permitted = r#"{"error":"not-permitted","detail":"/etc"}"#;
        assert_eq!(field(not_permitted, "error"), Some("not-permitted"));
        assert_eq!(field(not_permitted, "detail"), Some("/etc"));

        let no_such_file = r#"{"error":"no-such-file","detail":"/tmp/missing.mov"}"#;
        assert_eq!(field(no_such_file, "error"), Some("no-such-file"));
    }
}
