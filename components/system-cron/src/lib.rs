//! `system-cron` — read the machine's scheduled jobs, for a component that
//! cannot read crontab
//!
//! The read happens in `comp-cron`, which is native because crontab is the
//! host's and a `wasm32-wasip2` guest has no such file (ADR-0095). This is the
//! component side: it holds the contract, and reaches the daemon over HTTP the
//! same way `fs-watcher` reaches `comp-fswatch`.
//!
//! What this may dial is a MANIFEST decision (ADR-0008). The daemon's address
//! is `cron-url` in `wasi:config`, and the deployment's egress allow-list
//! decides whether the call leaves at all. There is no allow-list on the
//! daemon side beyond that: there is only one crontab to read, unlike a
//! filesystem path that names somewhere.
//!
//! Config (wasi:config/store):
//!   cron-url    where `comp-cron` is listening, e.g. http://127.0.0.1:8008
//!
//! It used to return `format!("UNIMPLEMENTED: ...")` — a sentence shaped like
//! an answer, which no caller could tell from a real one. That is what the old
//! `-> string` contract permitted; the current one cannot express it.

#[allow(warnings)]
mod bindings;

use bindings::exports::os::system::cron::{CronError, Guest, Job};
use bindings::wasi::config::store as config;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme};
use bindings::wasi::io::streams::StreamError;

struct Component;

/// Ten seconds. Reading a crontab is a single process spawn; anything slower
/// than this is a daemon in trouble rather than a big crontab.
const TIMEOUT_NS: u64 = 10_000_000_000;

fn daemon_url() -> Result<String, CronError> {
    match config::get("cron-url") {
        Ok(Some(u)) if !u.is_empty() => Ok(u),
        // Unavailable rather than any other variant: nobody refused anything,
        // the deployment simply never said where the daemon is.
        _ => Err(CronError::Unavailable("cron-url is not set — this component has nowhere to ask".into())),
    }
}

fn parse_url(url: &str) -> Result<(Scheme, String, String), CronError> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (Scheme::Http, r)
    } else {
        return Err(CronError::Unavailable(format!("cron-url must be http(s), got {url:?}")));
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
fn post(body: Vec<u8>) -> Result<Vec<u8>, CronError> {
    let url = daemon_url()?;
    let (scheme, authority, base) = parse_url(&url)?;
    let net = |m: &str| CronError::Unavailable(m.to_string());

    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    let req = OutgoingRequest::new(headers);
    req.set_method(&Method::Post).map_err(|_| net("set method"))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net("set scheme"))?;
    req.set_authority(Some(&authority)).map_err(|_| net("set authority"))?;
    req.set_path_with_query(Some(&format!("{base}/list-jobs"))).map_err(|_| net("set path"))?;

    let out = req.body().map_err(|_| net("body"))?;
    {
        let stream = out.write().map_err(|_| net("write"))?;
        // Chunked: `blocking-write-and-flush` traps above 4096 bytes. The
        // body here is always small (`{}`), but the shape matches the rest of
        // this repo's HTTP clients.
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
/// The daemon's replies have two flat shapes and no nesting beyond a job
/// list, so a full serde dependency would be more code than the thing it
/// reads.
fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = &json[at..];
    let start = rest.find('"')? + 1;
    let end = rest[start..].find('"')? + start;
    Some(&rest[start..end])
}

fn jobs_of(json: &str) -> Vec<Job> {
    let mut out = Vec::new();
    let Some(list_at) = json.find("\"jobs\"") else { return out };
    for chunk in json[list_at..].split("{\"schedule\":").skip(1) {
        let obj = format!("{{\"schedule\":{chunk}");
        let Some(schedule) = field(&obj, "schedule") else { continue };
        let Some(command) = field(&obj, "command") else { continue };
        out.push(Job { schedule: schedule.to_string(), command: command.to_string() });
    }
    out
}

impl Guest for Component {
    fn list_jobs() -> Result<Vec<Job>, CronError> {
        let raw = post(b"{}".to_vec())?;
        let text = String::from_utf8_lossy(&raw).into_owned();

        if let Some(err) = field(&text, "error") {
            let detail = field(&text, "detail").unwrap_or_default().to_string();
            return Err(CronError::Unavailable(format!("{err}: {detail}")));
        }

        Ok(jobs_of(&text))
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_are_read_back_with_schedule_and_command() {
        let json = r#"{"jobs":[{"schedule":"0 3 * * *","command":"/usr/bin/backup.sh"},
                               {"schedule":"*/5 * * * *","command":"/usr/bin/ping.sh"}]}"#;
        let jobs = jobs_of(json);
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].schedule, "0 3 * * *");
        assert_eq!(jobs[0].command, "/usr/bin/backup.sh");
        assert_eq!(jobs[1].command, "/usr/bin/ping.sh");
    }

    /// An empty job list is a legitimate answer — no crontab installed is not
    /// a failure — and must not read as an error.
    #[test]
    fn an_empty_job_list_is_not_an_error() {
        let json = r#"{"jobs":[]}"#;
        assert!(jobs_of(json).is_empty());
        assert!(field(json, "error").is_none());
    }

    #[test]
    fn an_unavailable_daemon_carries_its_detail() {
        let json = r#"{"error":"unavailable","detail":"crontab: command not found"}"#;
        assert_eq!(field(json, "error"), Some("unavailable"));
        assert_eq!(field(json, "detail"), Some("crontab: command not found"));
    }
}
