//! `comp-uinotify` — raise a notification on the user's desktop, for a
//! component that cannot reach the notification bus.
//!
//! ## Why this is a process and not a component
//!
//! Raising a desktop notification needs the platform's notification bus —
//! D-Bus on Linux, native APIs on macOS/Windows — and a `wasm32-wasip2` guest
//! has none of that (ADR-0095). `components/ui-notifier` is the component
//! side: it holds the WIT contract and dials this over HTTP.
//!
//! ## What "unavailable" means here
//!
//! This needs a real, running notification bus. In a headless environment —
//! most CI, most servers, many containers — there isn't one, and `notify-rust`
//! reports that as an error. `unavailable` is the honest answer in that case,
//! not a bug: a notification with nowhere to appear was never going to be
//! delivered.
//!
//!   comp-uinotify --addr 127.0.0.1:8009

use anyhow::Result;
use axum::{routing::post, Json, Router};
use clap::Parser;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-uinotify", about = "Native daemon for ui-notifier")]
struct Args {
    /// Shared secret a caller must send as `Authorization: Bearer
    /// <token>`. Loopback binding alone is not a boundary — see
    /// `comp_reconciler::daemon_auth`'s own doc for why. No token means
    /// no check, logged loudly rather than silently.
    #[arg(long)]
    token: Option<String>,
    /// Same, but read from a file (a systemd `LoadCredential` path)
    /// rather than passed as a value — `--token` is `ps`-readable by
    /// any local user, which is most of what this exists to close.
    /// Wins over `--token` when both are given.
    #[arg(long)]
    token_file: Option<std::path::PathBuf>,

    /// Where to listen. Loopback by default: this has no authentication of
    /// its own.
    #[arg(long, default_value = "127.0.0.1:8009")]
    addr: String,
}

#[derive(Deserialize)]
struct NotifyReq {
    msg: String,
}

async fn handle(Json(req): Json<NotifyReq>) -> Json<Value> {
    match notify_rust::Notification::new().summary("Holon").body(&req.msg).show() {
        Ok(_) => Json(json!({ "ok": true })),
        Err(e) => Json(json!({ "error": "unavailable", "detail": e.to_string() })),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token = comp_reconciler::daemon_auth::resolve_token(args.token.clone(), args.token_file.clone());
    comp_reconciler::daemon_auth::warn_if_unauthenticated("comp-uinotify", &token);
    println!("comp-uinotify: listening on http://{}", args.addr);
    let app = Router::new().route("/notify", post(handle))
        .layer(axum::middleware::from_fn(comp_reconciler::daemon_auth::require_token))
        .layer(axum::Extension(std::sync::Arc::new(token)));
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The request body round-trips through serde the same way the
    /// component's own JSON does — a quote or backslash in the message must
    /// survive intact rather than breaking the shape.
    #[test]
    fn a_message_with_quotes_round_trips() {
        let body = r#"{"msg":"say \"hi\""}"#;
        let req: NotifyReq = serde_json::from_str(body).expect("valid json");
        assert_eq!(req.msg, r#"say "hi""#);
    }
}
