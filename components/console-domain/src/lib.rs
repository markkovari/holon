//! `console-domain` — the Holon console (see `wit/console.wit` for why it is a
//! second client of the platform API rather than a second control plane).
//!
//! Slice one: the shell, and one real write path end to end.
//!
//!   GET  /                        the SPA
//!   POST /api/session             log in — forwarded to the platform, cookie back
//!   GET  /api/session             who the cookie belongs to
//!   DELETE /api/session           log out
//!   GET  /api/projects            proxied
//!   GET  /api/projects/{p}/goals  proxied
//!   POST /api/goals               AUTHOR a goal: a pull request, then a queue entry
//!
//! ## The session is a cookie, and the platform never sees a cookie
//!
//! The platform speaks bearer tokens — that is what the CLI stores. A browser
//! should not: this page renders model-written prose (goal specs, lessons,
//! diffs), and a token any script can read is a bad pairing with a page showing
//! output an agent can be influenced into producing. So the console holds the
//! token in an `HttpOnly` cookie and puts it on the `Authorization` header of
//! every call it forwards. The token never reaches JavaScript.
//!
//! ## Authoring a goal is two writes, and the order matters
//!
//! A goal is prose in git plus a row in the platform (ADR-0082: the spec belongs
//! in git, versioned and content-addressed for free). Authoring one from a
//! browser is therefore a pull request AND a queue entry.
//!
//! The pull request goes first. If the queue entry fails, the result is an open
//! PR nobody queued — visible, revertable, and obviously incomplete. The other
//! order leaves a queue entry pointing at a spec path that does not exist, which
//! looks fine until something tries to run it.
//!
//! The entry is created `queued` and nothing starts it. That is ADR-0082's whole
//! stance and this UI does not get to relax it: starting a run spends money and
//! opens pull requests, so it stays a deliberate act.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Value};

use bindings::git::forge::repo as forge;
use bindings::knowledge::graph::store as graph;
use bindings::ui::assets::files as statics;
use bindings::wasi::config::store as config;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::outgoing_handler;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingRequest, OutgoingResponse,
    RequestOptions, ResponseOutparam, Scheme,
};

struct Component;

/// The cookie the browser holds. `HttpOnly` so no script can read it, `SameSite=Strict`
/// because every request this cookie authorises is a same-origin call from our own SPA.
const SESSION_COOKIE: &str = "holon_session";

/// Where a goal spec lands in the repository. `.comp/goals/` is where the CLI
/// already expects to find one.
const DEFAULT_GOALS_DIR: &str = ".comp/goals";

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Post, ["api", "session"]) => log_in(&request),
            (Method::Get, ["api", "session"]) => whoami(&request),
            (Method::Delete, ["api", "session"]) => log_out(),

            (Method::Get, ["api", "projects"]) => proxy_get(&request, "/api/projects"),
            (Method::Get, ["api", "projects", p, "goals"]) => {
                proxy_get(&request, &format!("/api/projects/{p}/goals"))
            }

            (Method::Post, ["api", "goals"]) => author_goal(&request),

            (Method::Get, ["api", "runs"]) => runs(),
            (Method::Get, ["api", "runs", id]) => run_detail(&percent_decode(id)),

            // Anything else that is a GET is the SPA — client-side routes render
            // the shell. API routes are matched above, so this cannot swallow one.
            (Method::Get, _) => serve_static(&route),
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
    /// A body with headers of its own — the SPA's bytes, or a Set-Cookie.
    Raw(u16, Vec<(String, String)>, Vec<u8>),
    Err(u16, String),
}

// ---- session ---------------------------------------------------------------

/// Forward a login to the platform and keep the token server-side.
///
/// The body is passed through unchanged rather than re-encoded: the platform owns
/// what a credential looks like, and a console that reshapes it would have to be
/// changed every time the platform's login grows a field.
fn log_in(request: &IncomingRequest) -> Outcome {
    let body = match read_body(request) {
        Ok(b) => b,
        Err(_) => return Outcome::Err(400, "could not read body".into()),
    };
    let (status, answer) = match platform_call("POST", "/api/login", None, Some(&body)) {
        Ok(pair) => pair,
        Err(e) => return Outcome::Err(502, e),
    };
    if !(200..300).contains(&status) {
        // Pass the platform's refusal through as-is. A console that rewrites
        // "wrong password" into its own words is a console that will disagree
        // with the CLI about what happened.
        return Outcome::Raw(
            status,
            vec![("content-type".into(), "application/json".into())],
            answer,
        );
    }

    let parsed: Value = serde_json::from_slice(&answer).unwrap_or(Value::Null);
    let token = parsed["token"].as_str().unwrap_or_default().to_string();
    if token.is_empty() {
        return Outcome::Err(502, "the platform accepted the login but returned no token".into());
    }

    // The token goes in the cookie and NOT in the response body — the whole
    // point of the exchange is that the browser cannot read it.
    Outcome::Raw(
        200,
        vec![
            ("content-type".into(), "application/json".into()),
            ("set-cookie".into(), session_cookie(&token)),
        ],
        json!({ "ok": true, "subject": parsed["subject"] }).to_string().into_bytes(),
    )
}

fn session_cookie(token: &str) -> String {
    // No `Secure` here: it would make the cookie useless over the plain-HTTP
    // loopback this runs on in development, and the deployment that terminates
    // TLS is the thing that should add it. Stated rather than silently omitted.
    format!("{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/")
}

fn whoami(request: &IncomingRequest) -> Outcome {
    match proxy_get(request, "/api/me") {
        // A 401 from the platform is not an error here — it is the answer to
        // "am I logged in", and the SPA renders a login form rather than a fault.
        Outcome::Raw(401, _, _) | Outcome::Err(401, _) => {
            Outcome::Json(200, json!({ "authenticated": false }).to_string())
        }
        other => other,
    }
}

fn log_out() -> Outcome {
    Outcome::Raw(
        200,
        vec![
            ("content-type".into(), "application/json".into()),
            // Expire it. The platform's own session outlives this, which is
            // correct: the CLI's login is not this browser's to end.
            (
                "set-cookie".into(),
                format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"),
            ),
        ],
        json!({ "ok": true }).to_string().into_bytes(),
    )
}

// ---- authoring a goal ------------------------------------------------------

/// `POST /api/goals {project, title, spec, priority?}`
///
/// Opens a pull request adding the spec, then queues the goal against the path
/// that PR creates. See the module header for why that order.
fn author_goal(request: &IncomingRequest) -> Outcome {
    let Some(token) = session_token(request) else {
        return Outcome::Err(401, "not signed in".into());
    };
    let body = match read_body(request).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
        Some(v) => v,
        None => return Outcome::Err(400, "expected a json body".into()),
    };

    let project = body["project"].as_str().unwrap_or_default().trim().to_string();
    let title = body["title"].as_str().unwrap_or_default().trim().to_string();
    let spec = body["spec"].as_str().unwrap_or_default().to_string();
    if project.is_empty() || title.is_empty() || spec.trim().is_empty() {
        return Outcome::Err(400, "project, title and spec are all required".into());
    }

    let slug = slugify(&title);
    if slug.is_empty() {
        // A title of nothing but punctuation would otherwise become `.md` at the
        // root of the goals directory.
        return Outcome::Err(400, "the title has no characters usable in a filename".into());
    }
    let path = format!("{}/{slug}.md", goals_dir());

    // 1. The pull request. `propose` is one call on purpose — the intermediate
    //    states (a branch with no commit) are litter if we die halfway.
    let proposal = forge::Proposal {
        branch: format!("goal/{slug}"),
        base: String::new(), // the repository's default
        title: format!("goal: {title}"),
        body: format!(
            "Proposed from the Holon console.\n\nQueued against `{path}` in project \
             `{project}`. Nothing runs until someone starts it (ADR-0082).\n"
        ),
        message: format!("goal: {title}"),
        changes: vec![forge::FileChange { path: path.clone(), content: spec_document(&title, &spec) }],
    };
    let opened = match forge::propose(&proposal) {
        Ok(o) => o,
        Err(e) => return forge_error(e),
    };

    // 2. The queue entry, pointing at the path the PR creates. `queued`, never
    //    started: what spends money stays a deliberate human act.
    let entry = json!({
        "title": title,
        "spec": path,
        "priority": body["priority"],
    });
    let queued = platform_call(
        "POST",
        &format!("/api/projects/{project}/goals"),
        Some(&token),
        Some(entry.to_string().as_bytes()),
    );

    match queued {
        Ok((status, answer)) if (200..300).contains(&status) => {
            let goal: Value = serde_json::from_slice(&answer).unwrap_or(Value::Null);
            Outcome::Json(
                201,
                json!({
                    "goal": goal,
                    "spec": path,
                    "pullRequest": { "number": opened.number, "url": opened.url },
                })
                .to_string(),
            )
        }
        // The PR is open and the entry is not. Say so precisely — the operator's
        // move is to queue it by hand or close the PR, and either needs the URL.
        Ok((status, answer)) => Outcome::Err(
            502,
            format!(
                "the pull request opened at {} but the goal was not queued ({status}): {}",
                opened.url,
                String::from_utf8_lossy(&answer).chars().take(200).collect::<String>()
            ),
        ),
        Err(e) => Outcome::Err(
            502,
            format!("the pull request opened at {} but the goal was not queued: {e}", opened.url),
        ),
    }
}

/// The file that lands in the repository.
///
/// A heading and the prose, nothing else. Deliberately not a generated header
/// block with a timestamp or an author: the file is reviewed in a pull request
/// and read by a model, and both are better served by the text somebody wrote
/// than by metadata the graph already holds.
fn spec_document(title: &str, spec: &str) -> String {
    format!("# {title}\n\n{}\n", spec.trim())
}

/// A filename from a title. Lowercase, alphanumerics and single dashes.
fn slugify(title: &str) -> String {
    let mut out = String::new();
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_matches('-').chars().take(60).collect()
}

fn forge_error(e: forge::ForgeError) -> Outcome {
    match e {
        // The branch already exists — the caller's move is a different title,
        // not a different request, so this is not a 400.
        forge::ForgeError::Conflict(m) => Outcome::Err(409, format!("a goal by that name is already proposed: {m}")),
        forge::ForgeError::NotConfigured(m) => {
            Outcome::Err(503, format!("no repository or token configured for the forge: {m}"))
        }
        forge::ForgeError::Rejected(m) => Outcome::Err(422, format!("the forge refused it: {m}")),
        forge::ForgeError::Unavailable(m) => Outcome::Err(502, format!("the forge is unreachable: {m}")),
    }
}

// ---- runs (ADR-0092) --------------------------------------------------------

/// The runs, newest first.
///
/// Reads the merged store directly rather than through the platform: run history
/// is the accumulated half that the control plane deliberately cannot see
/// (ADR-0091). The console is the one place both halves are on screen together.
fn runs() -> Outcome {
    // A bounded list. Runs accumulate forever and nobody scrolls past the last
    // few dozen; an unbounded SELECT here is the query that gets slow silently.
    //
    // `started_at` must stay in the projection: SurrealDB v3 refuses an ORDER BY
    // on a field the statement does not select, with a 400 rather than an
    // unordered result. Trimming this list to "just what the UI shows" would
    // break the query, not merely the sort.
    let surql = "SELECT id_text, goal, outcome, winner, url, branches, started_at, resolved_at \
                 FROM run ORDER BY started_at DESC LIMIT 50;";
    match graph::query(&surql.to_string()) {
        Ok(answer) => Outcome::Raw(
            200,
            vec![("content-type".into(), "application/json".into())],
            wrap(&answer, "runs"),
        ),
        Err(e) => graph_error(e),
    }
}

/// How many events one run detail may return.
///
/// A long agentic run emits events per attempt, per repair, per generation. This
/// was unbounded and a 5,000-event run returned all 5,000 — found by the browser
/// suite, which seeded exactly that. The cap is not the interesting part; the
/// COUNT beside it is, because a truncated timeline that does not say it is
/// truncated is the failure this repository keeps re-learning (ADR-0080, and
/// every `read_body` comment here).
const EVENT_PAGE: usize = 500;

/// One run: the node, its attempts, and its events in order.
///
/// Four statements in one request rather than four round trips — the run page
/// wants all of it at once, and a partially-loaded timeline is worse than a slow
/// one because it looks complete.
fn run_detail(id: &str) -> Outcome {
    let quoted = surql_string(id);
    let surql = format!(
        "SELECT * FROM run WHERE id_text = {quoted};\n\
         SELECT * FROM attempt WHERE run = {quoted} ORDER BY started_at;\n\
         SELECT count() FROM event WHERE run = {quoted} GROUP ALL;\n\
         SELECT * FROM event WHERE run = {quoted} ORDER BY at LIMIT {EVENT_PAGE};\n\
         SELECT name, path, added_at FROM capability WHERE added_by = {quoted} ORDER BY added_at;"
    );
    match graph::query(&surql) {
        Ok(answer) => Outcome::Raw(
            200,
            vec![("content-type".into(), "application/json".into())],
            detail(&answer),
        ),
        Err(e) => graph_error(e),
    }
}

/// `knowledge:graph/query` answers SurrealDB's per-statement envelope. The SPA
/// wants the rows, so unwrap the LAST statement's result under `key`.
fn wrap(answer: &str, key: &str) -> Vec<u8> {
    let rows = statements(answer).pop().unwrap_or(Value::Array(vec![]));
    json!({ key: rows }).to_string().into_bytes()
}

/// The five statements of a run detail, named.
///
/// `eventCount` is the total in the store and `events` is the first page of it.
/// Both are sent so the page can say "the first 500 of 5,000" — a timeline that
/// silently stops at 500 looks like a run that stopped at 500.
fn detail(answer: &str) -> Vec<u8> {
    let mut s = statements(answer);
    // Pop in reverse: the last statement is the capabilities.
    let capabilities = s.pop().unwrap_or(Value::Array(vec![]));
    let events = s.pop().unwrap_or(Value::Array(vec![]));
    let counted = s.pop().unwrap_or(Value::Array(vec![]));
    let attempts = s.pop().unwrap_or(Value::Array(vec![]));
    let run = s.pop().unwrap_or(Value::Array(vec![]));

    let shown = events.as_array().map(|a| a.len()).unwrap_or(0);
    // `count()` on an empty table answers with no rows at all, not a zero — so
    // fall back to what was actually returned rather than reporting 0 events
    // beside a list that has some.
    let total = counted[0]["count"].as_u64().unwrap_or(shown as u64);

    json!({
        "run": run.get(0).cloned().unwrap_or(Value::Null),
        "attempts": attempts,
        "events": events,
        "eventCount": total,
        "truncated": total > shown as u64,
        // What the pool gained (ADR-0089). Usually empty, which is the honest
        // answer: most runs change an app, and only some leave the system able
        // to do something it could not do before.
        "capabilities": capabilities,
    })
    .to_string()
    .into_bytes()
}

/// Per-statement results, in order. An unreadable answer is an empty list rather
/// than a panic: the run view showing nothing is recoverable, the component
/// trapping is not.
fn statements(answer: &str) -> Vec<Value> {
    serde_json::from_str::<Value>(answer)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .map(|a| a.into_iter().map(|s| s["result"].clone()).collect())
        .unwrap_or_default()
}

/// Enough percent-decoding for a run id in a path segment.
///
/// A run id is `seed/g1/branch` (ADR-0078), so the SPA sends it
/// `encodeURIComponent`'d and the `/` arrives as `%2F`. `path-with-query` hands
/// the component the RAW path — nothing decodes it on the way in — so without
/// this the id is the literal `77%2Fg1` and every run detail is empty while the
/// list beside it works. `studio-domain` carries the same helper for the same
/// reason.
fn percent_decode(s: &str) -> String {
    let bytes = s.replace('+', " ").into_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&String::from_utf8_lossy(&bytes[i + 1..i + 3]), 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// A SurrealQL string literal. Through JSON so a value cannot carry syntax —
/// this one takes a run id straight off the URL (ADR-0080).
fn surql_string(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

fn graph_error(e: graph::GraphError) -> Outcome {
    match e {
        // No credentials is an operator problem, not a caller's — say which.
        graph::GraphError::NotConfigured(m) => {
            Outcome::Err(503, format!("no knowledge store configured: {m}"))
        }
        graph::GraphError::Unavailable(m) => Outcome::Err(502, format!("the knowledge store is unreachable: {m}")),
        graph::GraphError::Rejected(m) => Outcome::Err(502, format!("the knowledge store refused the read: {m}")),
    }
}

// ---- the platform, as a client ---------------------------------------------

fn platform_url() -> String {
    config::get("platform-url")
        .ok()
        .flatten()
        .unwrap_or_else(|| "http://127.0.0.1:8080".to_string())
}

fn goals_dir() -> String {
    config::get("goals-dir")
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_GOALS_DIR.to_string())
        .trim_matches('/')
        .to_string()
}

/// Forward a GET, carrying the caller's session as a bearer token.
fn proxy_get(request: &IncomingRequest, path: &str) -> Outcome {
    let token = session_token(request);
    match platform_call("GET", path, token.as_deref(), None) {
        Ok((status, body)) => {
            Outcome::Raw(status, vec![("content-type".into(), "application/json".into())], body)
        }
        Err(e) => Outcome::Err(502, e),
    }
}

/// The session token, from the cookie. `None` means not signed in.
fn session_token(request: &IncomingRequest) -> Option<String> {
    let raw = header(request, "cookie")?;
    raw.split(';').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == SESSION_COOKIE).then(|| v.trim().to_string())
    })
}

/// One call to `platform-domain`, carrying the caller's token if there is one.
///
/// Returns the status alongside the body rather than mapping it: the console is a
/// proxy here, and a 401 or a 409 from the control plane means the same thing to
/// the browser as it does to the CLI. Collapsing them into an error type would
/// make the two clients disagree about what happened.
fn platform_call(
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&[u8]>,
) -> Result<(u16, Vec<u8>), String> {
    let url = format!("{}{}", platform_url().trim_end_matches('/'), path);
    let (scheme, authority, full_path) = parse_url(&url)?;

    let headers = Fields::new();
    let _ = headers.set(&"accept".to_string(), &[b"application/json".to_vec()]);
    if let Some(b) = body {
        let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
        // Framed by length rather than chunked: a chunked body is legal and a
        // real edge can still mishandle it, and the symptom is a truncated
        // response that a loopback test never reproduces.
        let _ = headers.set(&"content-length".to_string(), &[b.len().to_string().into_bytes()]);
    }
    // Close after the response, so the whole body is delivered rather than the
    // host being left reading a keep-alive socket that never signals end.
    let _ = headers.set(&"connection".to_string(), &[b"close".to_vec()]);
    if let Some(t) = token {
        let _ = headers.set(&"authorization".to_string(), &[format!("Bearer {t}").into_bytes()]);
    }

    let req = OutgoingRequest::new(headers);
    let m = match method {
        "POST" => Method::Post,
        "DELETE" => Method::Delete,
        _ => Method::Get,
    };
    req.set_method(&m).map_err(|_| "set method".to_string())?;
    req.set_scheme(Some(&scheme)).map_err(|_| "set scheme".to_string())?;
    req.set_authority(Some(&authority)).map_err(|_| "set authority".to_string())?;
    req.set_path_with_query(Some(&full_path)).map_err(|_| "set path".to_string())?;

    {
        let out = req.body().map_err(|_| "body".to_string())?;
        if let Some(b) = body {
            {
                let stream = out.write().map_err(|_| "write stream".to_string())?;
                if !write_all(&stream, b) {
                    return Err("writing the request body".into());
                }
            }
        }
        OutgoingBody::finish(out, None).map_err(|_| "finish body".to_string())?;
    }

    // Set all three explicitly. An unset default that happens to be short is how
    // a call that should take 200ms dies as "data receipt timed out".
    let opts = RequestOptions::new();
    let _ = opts.set_connect_timeout(Some(10_000_000_000)); // 10s
    let _ = opts.set_first_byte_timeout(Some(30_000_000_000)); // 30s
    let _ = opts.set_between_bytes_timeout(Some(30_000_000_000)); // 30s

    let future = outgoing_handler::handle(req, Some(opts))
        .map_err(|e| format!("the platform could not be called: {e:?}"))?;
    future.subscribe().block();
    let resp = future
        .get()
        .ok_or_else(|| "no response from the platform".to_string())?
        .map_err(|_| "response already taken".to_string())?
        .map_err(|e| format!("the platform is unreachable: {e:?}"))?;

    let status = resp.status();
    let mut buf = Vec::new();
    if let Ok(incoming) = resp.consume() {
        if let Ok(stream) = incoming.stream() {
            loop {
                match stream.blocking_read(8192) {
                    Ok(c) if c.is_empty() => break,
                    Ok(c) => buf.extend_from_slice(&c),
                    // Both arms break, unlike the REQUEST side: here a short read
                    // is reported with the status the server already sent, and the
                    // caller sees a body it can judge. Erroring would discard a
                    // status that is the more useful half of the answer.
                    Err(_) => break,
                }
            }
        }
    }
    Ok((status, buf))
}

/// `scheme://authority/path?query`, split for `OutgoingRequest`.
fn parse_url(url: &str) -> Result<(Scheme, String, String), String> {
    let (scheme, rest) = match url.split_once("://") {
        Some(("https", r)) => (Scheme::Https, r),
        Some(("http", r)) => (Scheme::Http, r),
        _ => return Err(format!("platform-url must start with http:// or https://, got {url:?}")),
    };
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a.to_string(), format!("/{p}")),
        None => (rest.to_string(), "/".to_string()),
    };
    Ok((scheme, authority, path))
}

// ---- http plumbing ---------------------------------------------------------

/// Write a whole body, however long it is.
///
/// `blocking-write-and-flush` accepts at most 4096 bytes and TRAPS above that
/// rather than returning an error: the component dies mid-response and the caller
/// sees `connection closed before message completed`, three layers from the
/// cause. The SPA's bundle is far past that, so this is not optional here.
fn write_all(stream: &bindings::wasi::io::streams::OutputStream, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        let ready = match stream.check_write() {
            Ok(0) => {
                stream.subscribe().block();
                continue;
            }
            Ok(n) => n as usize,
            Err(_) => return false,
        };
        let take = ready.min(bytes.len());
        if stream.write(&bytes[..take]).is_err() {
            return false;
        }
        bytes = &bytes[take..];
    }
    stream.blocking_flush().is_ok()
}

/// Serve the baked SPA via `ui:assets`: exact path, else fall back to index.html
/// so client-side routes render the shell.
fn serve_static(route: &str) -> Outcome {
    let want = if route == "/" { "/index.html" } else { route };
    match statics::get(want).or_else(|| statics::get("/index.html")) {
        Some(a) => Outcome::Raw(200, vec![("content-type".into(), a.content_type)], a.body),
        None => Outcome::Err(404, "not_found".into()),
    }
}

/// The most a request body may be, before the component stops reading it.
///
/// A goal spec is prose; sixteen megabytes of it is not a goal. A ceiling rather
/// than a policy: past this the read stops and the caller is told, instead of
/// growing until the guest hits wasmtime's per-store memory cap and TRAPS, which
/// reaches the caller as a closed connection saying nothing about a size.
const MAX_BODY_BYTES: usize = 1024 * 1024;

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let body = request.consume().map_err(|_| ())?;
    let stream = body.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                if buf.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err(());
                }
                buf.extend_from_slice(&chunk);
            }
            // `Closed` is end-of-body; anything else is a read that went wrong.
            // Collapsing both into `break` returns a TRUNCATED body as if it were
            // complete — for a goal spec that means proposing half a document.
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            Err(_) => return Err(()),
        }
    }
    Ok(buf)
}

fn header(request: &IncomingRequest, name: &str) -> Option<String> {
    request
        .headers()
        .get(&name.to_string())
        .into_iter()
        .find_map(|v| String::from_utf8(v).ok())
}

fn emit(response_out: ResponseOutparam, result: Outcome) {
    let (code, header_pairs, body) = match result {
        Outcome::Json(c, b) => (
            c,
            vec![("content-type".to_string(), "application/json".to_string())],
            b.into_bytes(),
        ),
        Outcome::Raw(c, h, b) => (c, h, b),
        Outcome::Err(c, m) => (
            c,
            vec![("content-type".to_string(), "application/json".to_string())],
            json!({ "error": m }).to_string().into_bytes(),
        ),
    };
    let headers = Fields::new();
    for (k, v) in &header_pairs {
        let _ = headers.set(k, &[v.as_bytes().to_vec()]);
    }
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(code);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        write_all(&stream, &body);
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
