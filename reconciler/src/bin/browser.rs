//! `comp-browser` — render a page, for a component that cannot start a browser.
//!
//! ## Why this is a process and not a component
//!
//! Driving a real browser needs a process — Chrome forks renderer subprocesses
//! of its own — and a `wasm32-wasip2` guest has none of that (ADR-0095). So the
//! part that needs an operating system is native and is reached over HTTP
//! exactly like the filesystem watcher and the checks runner are.
//!
//! `components/browser-automation` is the component side: it holds the WIT
//! contract and dials this. Nothing here knows what a goal is.
//!
//! ## An allow-list, for the same reason `comp-fswatch` has one
//!
//! The url in a request can come from a model. A url is a way to make this
//! process fetch (and execute the JavaScript of) anywhere on the internet, so
//! a request names a url and this refuses it unless its host was listed.
//! `--allow-host example.com` permits `example.com` and any subdomain of it
//! (`www.example.com`, `a.b.example.com`) — a suffix match on the labels of
//! the host, not a substring match, so `example.com.evil.com` (whose host ends
//! in the bytes "example.com" but has "evil.com" as its actual registered
//! domain) is refused rather than let through by a naive `ends_with`.
//! Nothing is permitted by default; a daemon started with no `--allow-host`
//! refuses everything, which is the correct behaviour for a capability nobody
//! has scoped yet.
//!
//! ## What this needs on the host, and what happens when it is missing
//!
//! `headless_chrome::Browser::default()` launches a real Chrome or Chromium
//! binary. That binary is not part of this repository and is not installed in
//! every environment this daemon might run in (a CI sandbox with no display,
//! for instance). When launching or navigating fails, this reports
//! `unavailable` rather than crashing the daemon process — the honest answer
//! in a sandbox with no browser is "I could not do that", not a panic that
//! takes every other in-flight request down with it.
//!
//!   comp-browser --addr 127.0.0.1:8001 --allow-host example.com

use anyhow::Result;
use axum::{extract::State, routing::post, Json, Router};
use clap::Parser;
use headless_chrome::Browser;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-browser", about = "Render a page for a component that cannot start a browser.")]
struct Args {
    /// Where to listen. Loopback by default: this drives a real browser and
    /// has no authentication of its own.
    #[arg(long, default_value = "127.0.0.1:8001")]
    addr: String,

    /// A host this may navigate to, repeatable. Matches the host itself and
    /// any subdomain of it.
    ///
    /// An allow-list rather than trusting whatever a request names, for the
    /// same reason egress is an allow-list: the input comes from an agent.
    /// Empty means nothing is permitted, which is what a capability nobody
    /// has scoped should do.
    #[arg(long = "allow-host")]
    allow_host: Vec<String>,
}

/// Most of the daemon's reply this will send back. Matches the ceiling the
/// component buffers on its side (`MAX_REPLY_BYTES` in
/// `components/browser-automation/src/lib.rs`) — sending more than the other
/// side will read is a truncated answer either way, so it is truncated here
/// with a caller able to tell that happened rather than the component's read
/// loop failing outright.
const MAX_CONTENT_BYTES: usize = 2 * 1024 * 1024;

struct Daemon {
    allowed: Vec<String>,
}

#[derive(Deserialize)]
struct SnapshotReq {
    url: String,
}

impl Daemon {
    /// Is `host` the allow-listed name itself, or a subdomain of it?
    ///
    /// A suffix match on dot-separated labels, not on raw bytes: `ends_with`
    /// on the string would let `example.com` (allowed) match the host
    /// `evil-example.com`, and — the sharper version of the same mistake —
    /// `example.com.evil.com`, whose actual owner is `evil.com`. Requiring the
    /// character before the match to be a label boundary (`.`, or nothing at
    /// all) closes both.
    fn permits(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        self.allowed.iter().any(|allowed| {
            let allowed = allowed.to_ascii_lowercase();
            host == allowed || host.ends_with(&format!(".{allowed}"))
        })
    }
}

/// The host a url names, lowercased, with any port stripped. `None` if the
/// url has no scheme this understands or no host at all.
fn host_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split('/').next().unwrap_or(rest);
    let host = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

async fn snapshot(State(d): State<std::sync::Arc<Daemon>>, Json(req): Json<SnapshotReq>) -> Json<Value> {
    let Some(host) = host_of(&req.url) else {
        return Json(json!({ "error": "unavailable", "detail": format!("not a valid http(s) url: {}", req.url) }));
    };
    if !d.permits(&host) {
        return Json(json!({ "error": "not-permitted", "detail": host }));
    }

    // Chrome is launched, driven and torn down inside one blocking call: a
    // browser instance held across requests would leak tabs and state between
    // callers, and this is not a latency-sensitive path.
    let url = req.url.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let browser = Browser::default().map_err(|e| format!("launch: {e}"))?;
        let tab = browser.new_tab().map_err(|e| format!("new tab: {e}"))?;
        tab.navigate_to(&url).map_err(|e| format!("navigate: {e}"))?;
        tab.wait_until_navigated().map_err(|e| format!("wait: {e}"))?;
        tab.get_content().map_err(|e| format!("content: {e}"))
    })
    .await;

    match result {
        Ok(Ok(mut content)) => {
            content.truncate(MAX_CONTENT_BYTES);
            Json(json!({ "content": content }))
        }
        // Any navigation or launch failure is unavailable and retryable — it
        // says nothing about whether the host was allowed, only that this
        // attempt did not produce a page.
        Ok(Err(detail)) => Json(json!({ "error": "unavailable", "detail": detail })),
        Err(join_err) => Json(json!({ "error": "unavailable", "detail": format!("task panicked: {join_err}") })),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.allow_host.is_empty() {
        eprintln!(
            "comp-browser: no --allow-host given, so every request will be refused. \
             That is deliberate — a browser nobody has scoped visits nowhere."
        );
    }
    let allowed = args.allow_host.clone();
    println!("comp-browser: listening on http://{} | {} allowed host(s)", args.addr, allowed.len());
    let state = std::sync::Arc::new(Daemon { allowed });
    let app = Router::new().route("/snapshot", post(snapshot)).with_state(state);
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daemon(allow: &[&str]) -> Daemon {
        Daemon { allowed: allow.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn a_url_host_is_extracted_without_its_scheme_port_or_path() {
        assert_eq!(host_of("https://example.com/a/b"), Some("example.com".into()));
        assert_eq!(host_of("http://example.com:8080/x"), Some("example.com".into()));
        assert_eq!(host_of("https://EXAMPLE.com"), Some("example.com".into()));
        assert_eq!(host_of("not-a-url"), None);
    }

    /// The allowed host and its subdomains match; a host that merely ends in
    /// the same bytes does not.
    #[test]
    fn a_lookalike_host_cannot_pass_as_an_allowed_one() {
        let d = daemon(&["example.com"]);
        assert!(d.permits("example.com"), "the listed host itself");
        assert!(d.permits("www.example.com"), "a real subdomain");
        assert!(!d.permits("example.com.evil.com"), "a host that merely ends in the allowed bytes");
        assert!(!d.permits("evil-example.com"), "a host with the allowed name as a suffix of a longer label");
        assert!(!d.permits("evil.com"), "an unrelated host");
    }

    #[test]
    fn an_unscoped_daemon_permits_nothing() {
        let d = daemon(&[]);
        assert!(!d.permits("example.com"));
    }
}
