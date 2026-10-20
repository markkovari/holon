//! Holon console. The token lives in an HttpOnly cookie, never in JS: this page renders model prose.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/ui-assets",
            "../git-forge/wit",
            "../../host/wit/deps/comp-secrets",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../knowledge-graph/wit",
            "wit",
        ],
        world: "console:app/console-domain",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use serde_json::{json, Value};

use bindings::git::forge::repo as forge;
use bindings::knowledge::graph::store as graph;
use bindings::ui::assets::files as statics;
use bindings::wasi::config::store as config;

use bindings::p3::handler::Guest;
use bindings::p3::http::types::{
    ErrorCode, Fields, Method, Request, RequestOptions, Response, Scheme,
};

struct Component;

/// `HttpOnly` so no script reads it; `SameSite=Strict` since only our own SPA calls with it.
const SESSION_COOKIE: &str = "holon_session";

const DEFAULT_GOALS_DIR: &str = ".comp/goals";

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let method = request.get_method();
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Post, ["api", "session"]) => log_in(request).await,
            (Method::Get, ["api", "session"]) => whoami(&request).await,
            (Method::Delete, ["api", "session"]) => log_out(),

            (Method::Get, ["api", "projects"]) => proxy_get(&request, "/api/projects").await,
            (Method::Get, ["api", "projects", p, "goals"]) => {
                proxy_get(&request, &format!("/api/projects/{p}/goals")).await
            }

            (Method::Post, ["api", "goals"]) => author_goal(request).await,
            // Forwarded verbatim: platform-domain's `goal_may` decides which moves are legal.
            (Method::Post, ["api", "goals", id, action])
                if matches!(*action, "start" | "review" | "done" | "fail") =>
            {
                proxy_post(request, &format!("/api/goals/{}/{action}", percent_decode(id))).await
            }

            (Method::Get, ["api", "runs"]) => runs(),
            (Method::Get, ["api", "runs", id]) => run_detail(&percent_decode(id)),

            // Client-side routes render the shell; API routes match above, so none is swallowed.
            (Method::Get, _) => serve_static(&route),
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(outcome)
    }
}

enum Outcome {
    Json(u16, String),
    Raw(u16, Vec<(String, String)>, Vec<u8>),
    Err(u16, String),
}

/// The body passes through unchanged: the platform owns what a credential looks like.
async fn log_in(request: Request) -> Outcome {
    let body = match read_body(request).await {
        Ok(b) => b,
        Err(_) => return Outcome::Err(400, "could not read body".into()),
    };
    let (status, answer) = match platform_call("POST", "/api/login", None, Some(&body)).await {
        Ok(pair) => pair,
        Err(e) => return Outcome::Err(502, e),
    };
    if !(200..300).contains(&status) {
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

    // The token goes in the cookie, never the body: the browser must not read it.
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
    // No `Secure`: it breaks the plain-HTTP dev loopback; the TLS-terminating deployment adds it.
    format!("{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/")
}

async fn whoami(request: &Request) -> Outcome {
    match proxy_get(request, "/api/me").await {
        // A 401 here is the answer to "am I logged in", not a fault.
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
            // The platform session outlives this: the CLI's login is not this browser's to end.
            (
                "set-cookie".into(),
                format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"),
            ),
        ],
        json!({ "ok": true }).to_string().into_bytes(),
    )
}

async fn author_goal(request: Request) -> Outcome {
    let Some(token) = session_token(&request) else {
        return Outcome::Err(401, "not signed in".into());
    };
    let body = match read_body(request)
        .await
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    {
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
        // Punctuation only would become `.md` at the goals root.
        return Outcome::Err(400, "the title has no characters usable in a filename".into());
    }
    let path = format!("{}/{slug}.md", goals_dir());

    // PR first (ADR-0082: the spec lives in git); one `propose` call, so a crash leaves no orphan branch.
    let proposal = forge::Proposal {
        branch: format!("goal/{slug}"),
        base: String::new(), // the repository's default
        title: format!("goal: {title}"),
        body: format!(
            "Proposed from the Holon console.\n\nQueued against `{path}` in project \
             `{project}`. Nothing runs until someone starts it (ADR-0082).\n"
        ),
        message: format!("goal: {title}"),
        changes: vec![forge::FileChange {
            path: path.clone(),
            content: spec_document(&title, &spec),
        }],
    };
    let opened = match forge::propose(&proposal) {
        Ok(o) => o,
        Err(e) => return forge_error(e),
    };

    // Queued, never started: what spends money stays a deliberate human act.
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
    )
    .await;

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

fn spec_document(title: &str, spec: &str) -> String {
    format!("# {title}\n\n{}\n", spec.trim())
}

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
        forge::ForgeError::Conflict(m) => {
            Outcome::Err(409, format!("a goal by that name is already proposed: {m}"))
        }
        forge::ForgeError::NotConfigured(m) => {
            Outcome::Err(503, format!("no repository or token configured for the forge: {m}"))
        }
        forge::ForgeError::Rejected(m) => Outcome::Err(422, format!("the forge refused it: {m}")),
        forge::ForgeError::Unavailable(m) => {
            Outcome::Err(502, format!("the forge is unreachable: {m}"))
        }
    }
}

/// Reads the merged store directly: run history is what the control plane cannot see (ADR-0091).
fn runs() -> Outcome {
    // SurrealDB v3 refuses ORDER BY on an unselected field, so `started_at` stays in the projection.
    let surql =
        "SELECT id_text, goal, spec, outcome, winner, url, branches, started_at, resolved_at \
                 FROM run ORDER BY started_at DESC LIMIT 50;";
    match graph::query(surql) {
        Ok(answer) => Outcome::Raw(
            200,
            vec![("content-type".into(), "application/json".into())],
            wrap(&answer, "runs"),
        ),
        Err(e) => graph_error(e),
    }
}

/// The count beside the cap says the timeline is truncated (ADR-0080).
const EVENT_PAGE: usize = 500;

/// One request: a partially-loaded timeline looks complete.
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

fn wrap(answer: &str, key: &str) -> Vec<u8> {
    let rows = statements(answer).pop().unwrap_or(Value::Array(vec![]));
    json!({ key: rows }).to_string().into_bytes()
}

fn detail(answer: &str) -> Vec<u8> {
    let mut s = statements(answer);
    // Pop in reverse: the last statement is the capabilities.
    let capabilities = s.pop().unwrap_or(Value::Array(vec![]));
    let events = s.pop().unwrap_or(Value::Array(vec![]));
    let counted = s.pop().unwrap_or(Value::Array(vec![]));
    let attempts = s.pop().unwrap_or(Value::Array(vec![]));
    let run = s.pop().unwrap_or(Value::Array(vec![]));

    let shown = events.as_array().map(|a| a.len()).unwrap_or(0);
    // `count()` on an empty table answers no rows, not zero.
    let total = counted[0]["count"].as_u64().unwrap_or(shown as u64);

    json!({
        "run": run.get(0).cloned().unwrap_or(Value::Null),
        "attempts": attempts,
        "events": events,
        "eventCount": total,
        "truncated": total > shown as u64,
        "capabilities": capabilities,
    })
    .to_string()
    .into_bytes()
}

/// Unreadable is empty, not a panic: an empty view is recoverable, a trap is not.
fn statements(answer: &str) -> Vec<Value> {
    serde_json::from_str::<Value>(answer)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .map(|a| a.into_iter().map(|s| s["result"].clone()).collect())
        .unwrap_or_default()
}

use guestfmt::percent_decode;

/// Through JSON so a value cannot carry syntax: this takes a run id off the URL (ADR-0080).
fn surql_string(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

fn graph_error(e: graph::GraphError) -> Outcome {
    match e {
        graph::GraphError::NotConfigured(m) => {
            Outcome::Err(503, format!("no knowledge store configured: {m}"))
        }
        graph::GraphError::Unavailable(m) => {
            Outcome::Err(502, format!("the knowledge store is unreachable: {m}"))
        }
        graph::GraphError::Rejected(m) => {
            Outcome::Err(502, format!("the knowledge store refused the read: {m}"))
        }
    }
}

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

async fn proxy_get(request: &Request, path: &str) -> Outcome {
    let token = session_token(request);
    match platform_call("GET", path, token.as_deref(), None).await {
        Ok((status, body)) => {
            Outcome::Raw(status, vec![("content-type".into(), "application/json".into())], body)
        }
        Err(e) => Outcome::Err(502, e),
    }
}

async fn proxy_post(request: Request, path: &str) -> Outcome {
    let token = session_token(&request);
    let body = read_body(request).await.unwrap_or_default();
    let body = (!body.is_empty()).then_some(body);
    match platform_call("POST", path, token.as_deref(), body.as_deref()).await {
        Ok((status, body)) => {
            Outcome::Raw(status, vec![("content-type".into(), "application/json".into())], body)
        }
        Err(e) => Outcome::Err(502, e),
    }
}

fn session_token(request: &Request) -> Option<String> {
    let raw = header(request, "cookie")?;
    raw.split(';').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == SESSION_COOKIE).then(|| v.trim().to_string())
    })
}

async fn platform_call(
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&[u8]>,
) -> Result<(u16, Vec<u8>), String> {
    let url = format!("{}{}", platform_url().trim_end_matches('/'), path);
    let (scheme, authority, full_path) = parse_url(&url)?;

    let headers = Fields::new();
    let _ = headers.set("accept", &[b"application/json".to_vec()]);
    if let Some(b) = body {
        let _ = headers.set("content-type", &[b"application/json".to_vec()]);
        // Length-framed, not chunked: a real edge can truncate chunked bodies; loopback never shows it.
        let _ = headers.set("content-length", &[b.len().to_string().into_bytes()]);
    }
    // Close after, so the host is not left reading a keep-alive socket that never signals end.
    let _ = headers.set("connection", &[b"close".to_vec()]);
    if let Some(t) = token {
        let _ = headers.set("authorization", &[format!("Bearer {t}").into_bytes()]);
    }

    // All three explicit: a short unset default kills a 200ms call as "data receipt timed out".
    let opts = RequestOptions::new();
    let _ = opts.set_connect_timeout(Some(10_000_000_000));
    let _ = opts.set_first_byte_timeout(Some(30_000_000_000));
    let _ = opts.set_between_bytes_timeout(Some(30_000_000_000));

    let (trailers_tx, trailers_rx) = bindings::wit_future::new(|| Ok(None));
    drop(trailers_tx);
    let contents = body.map(|b| {
        let b = b.to_vec();
        let (mut tx, rx) = bindings::wit_stream::new();
        wit_bindgen::spawn_local(async move {
            tx.write_all(b).await;
        });
        rx
    });
    let (req, _sent) = Request::new(headers, contents, trailers_rx, Some(opts));
    let m = match method {
        "POST" => Method::Post,
        "DELETE" => Method::Delete,
        _ => Method::Get,
    };
    req.set_method(&m).map_err(|_| "set method".to_string())?;
    req.set_scheme(Some(&scheme)).map_err(|_| "set scheme".to_string())?;
    req.set_authority(Some(&authority)).map_err(|_| "set authority".to_string())?;
    req.set_path_with_query(Some(&full_path)).map_err(|_| "set path".to_string())?;

    let resp = bindings::p3::http::client::send(req)
        .await
        .map_err(|e| format!("the platform is unreachable: {e:?}"))?;

    let status = resp.get_status_code();
    let (res_tx, res_rx) = bindings::wit_future::new(|| Ok(()));
    let (mut stream, trailers) = Response::consume_body(resp, res_rx);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.reserve(8192);
        let (st, b) = stream.read(buf).await;
        buf = b;
        match st {
            wit_bindgen::StreamResult::Complete(_) => {}
            wit_bindgen::StreamResult::Dropped => break,
            wit_bindgen::StreamResult::Cancelled => {
                return Err("the platform's response was cut short: cancelled".into())
            }
        }
    }
    drop(res_tx);
    // A transfer that failed partway is reported on the trailers, not by the stream ending.
    trailers.await.map_err(|e| format!("the platform's response was cut short: {e:?}"))?;
    Ok((status, buf))
}

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

guestio::guest_p3_respond!();

fn serve_static(route: &str) -> Outcome {
    let want = if route == "/" { "/index.html" } else { route };
    match statics::get(want).or_else(|| statics::get("/index.html")) {
        Some(a) => Outcome::Raw(200, vec![("content-type".into(), a.content_type)], a.body),
        None => Outcome::Err(404, "not_found".into()),
    }
}

/// A goal spec is prose; past this the read stops instead of trapping at the store's memory cap.
const MAX_BODY_BYTES: usize = 1024 * 1024;

guestio::guest_p3_read_body!(MAX_BODY_BYTES);

fn header(request: &Request, name: &str) -> Option<String> {
    request.get_headers().get(name).into_iter().find_map(|v| String::from_utf8(v).ok())
}

fn emit(result: Outcome) -> Result<Response, ErrorCode> {
    let (code, header_pairs, body) = match result {
        Outcome::Json(c, b) => {
            (c, vec![("content-type".to_string(), "application/json".to_string())], b.into_bytes())
        }
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
    respond_with(code, headers, body)
}

bindings::export!(Component with_types_in bindings);
