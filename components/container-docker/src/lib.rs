//! `container-docker` — list containers running on the machine
//!
//! Listing containers needs a container runtime socket, and a
//! `wasm32-wasip2` guest has none of those (ADR-0095). This is the component
//! side: it holds the contract, and reaches the daemon over HTTP the same
//! way `fs-watcher` reaches `comp-fswatch`.
//!
//! What this may dial is a MANIFEST decision (ADR-0008). The daemon's address
//! is `docker-url` in `wasi:config`, and the deployment's egress allow-list
//! decides whether the call leaves at all. A component that could reach any
//! address would make the allow-list decorative.
//!
//! Config (wasi:config/store):
//!   docker-url    where `comp-docker` is listening, e.g. http://127.0.0.1:8002
//!
//! It used to return `"UNIMPLEMENTED: ..."` — honest, but a `-> string`
//! contract cannot express failure any better than it can express success.
//! The current one can.

#[allow(warnings)]
mod bindings;

use bindings::exports::os::container::docker::{Container, DockerError, Guest};
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme};
use bindings::wasi::io::streams::StreamError;

struct Component;

/// Ten seconds. Listing containers is a local socket call on the daemon's
/// side; anything slower than this is a daemon in trouble rather than a lot
/// of containers, and a caller waiting on an HTTP request would rather hear
/// that than wait.
const TIMEOUT_NS: u64 = 10_000_000_000;

fn daemon_url() -> Result<String, DockerError> {
    match config::get("docker-url") {
        Ok(Some(u)) if !u.is_empty() => Ok(u),
        // Unavailable rather than not-permitted: nobody refused anything, the
        // deployment simply never said where the daemon is.
        _ => Err(DockerError::Unavailable(
            "docker-url is not set — this component has nowhere to ask".into(),
        )),
    }
}

fn parse_url(url: &str) -> Result<(Scheme, String, String), DockerError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(DockerError::Unavailable(format!("docker-url must be http(s), got {url:?}")));
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
fn post(body: Vec<u8>) -> Result<Vec<u8>, DockerError> {
    let url = daemon_url()?;
    let (scheme, authority, base) = parse_url(&url)?;
    let net = |m: &str| DockerError::Unavailable(m.to_string());

    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    let req = OutgoingRequest::new(headers);
    req.set_method(&Method::Post).map_err(|_| net("set method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net("set scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| net("set authority"))?;
    req.set_path_with_query(Some(&format!("{base}/call"))).map_err(|_| net("set path"))?;

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
/// The daemon's replies are a flat list of flat objects, so a full serde
/// dependency would be more code than the thing it reads.
fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = &json[at..];
    let start = rest.find('"')? + 1;
    let end = rest[start..].find('"')? + start;
    Some(&rest[start..end])
}

fn containers_of(json: &str) -> Vec<Container> {
    let mut out = Vec::new();
    let Some(list_at) = json.find("\"containers\"") else { return out };
    for chunk in json[list_at..].split("{\"id\":").skip(1) {
        let obj = format!("{{\"id\":{chunk}");
        let Some(id) = field(&obj, "id") else { continue };
        let image = field(&obj, "image").unwrap_or_default();
        let status = field(&obj, "status").unwrap_or_default();
        let name = field(&obj, "name").unwrap_or_default();
        out.push(Container {
            id: id.to_string(),
            image: image.to_string(),
            status: status.to_string(),
            name: name.to_string(),
        });
    }
    out
}

impl Guest for Component {
    fn ps() -> Result<Vec<Container>, DockerError> {
        let raw = post(b"{}".to_vec())?;
        let text = String::from_utf8_lossy(&raw).into_owned();

        // The daemon reports its refusals in the body rather than an HTTP
        // status, so a 200 is not itself a success.
        if let Some(err) = field(&text, "error") {
            let detail = field(&text, "detail").unwrap_or_default().to_string();
            return Err(DockerError::Unavailable(format!("{err}: {detail}")));
        }

        Ok(containers_of(&text))
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containers_are_read_back_with_all_four_fields() {
        let json = r#"{"containers":[
            {"id":"abc123def456","image":"nginx:latest","status":"Up 3 hours","name":"web"},
            {"id":"789xyz000111","image":"redis:7","status":"Up 1 day","name":"cache"}
        ]}"#;
        let cs = containers_of(json);
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].id, "abc123def456");
        assert_eq!(cs[0].image, "nginx:latest");
        assert_eq!(cs[0].status, "Up 3 hours");
        assert_eq!(cs[0].name, "web");
        assert_eq!(cs[1].name, "cache");
    }

    /// No containers running is a valid answer, not a failure.
    #[test]
    fn an_empty_list_is_not_an_error() {
        let json = r#"{"containers":[]}"#;
        assert!(containers_of(json).is_empty());
        assert!(field(json, "error").is_none());
    }

    #[test]
    fn an_unavailable_daemon_is_reported_as_unavailable() {
        let json = r#"{"error":"unavailable","detail":"connect: no such file or directory"}"#;
        assert_eq!(field(json, "error"), Some("unavailable"));
        assert_eq!(field(json, "detail"), Some("connect: no such file or directory"));
    }
}
