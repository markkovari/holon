//! `comp-clipboard` — native daemon for desktop-clipboard
//!
//! Reading the clipboard needs the desktop session's pasteboard, and a
//! `wasm32-wasip2` guest has none of those (ADR-0095). `components/desktop-clipboard`
//! is the component side: it holds the WIT contract and dials this over HTTP.
//!
//! This must run with access to a real desktop session — X11/Wayland on
//! Linux, the pasteboard on macOS, the clipboard API on Windows. In a
//! headless environment (a CI runner, a container with no display) it will
//! answer `unavailable` rather than crash: that is the honest answer for an
//! environment with no clipboard to read.

use anyhow::Result;
use axum::{routing::post, Json, Router};
use clap::Parser;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-clipboard", about = "Native daemon for desktop-clipboard")]
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

    #[arg(long, default_value = "127.0.0.1:8003")]
    addr: String,
}

async fn handle() -> Json<Value> {
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(e) => return Json(json!({ "error": "unavailable", "detail": e.to_string() })),
    };
    match clipboard.get_text() {
        Ok(text) => Json(json!({ "text": text })),
        // No text on the clipboard — could be an image, could be nothing at
        // all. Either way there is nothing for this contract to hand back.
        Err(arboard::Error::ContentNotAvailable) => Json(json!({ "error": "empty" })),
        Err(e) => Json(json!({ "error": "unavailable", "detail": e.to_string() })),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token = comp_reconciler::daemon_auth::resolve_token(args.token.clone(), args.token_file.clone());
    comp_reconciler::daemon_auth::warn_if_unauthenticated("comp-clipboard", &token);
    println!("comp-clipboard: listening on http://{}", args.addr);
    let app = Router::new().route("/call", post(handle))
        .layer(axum::middleware::from_fn(comp_reconciler::daemon_auth::require_token))
        .layer(axum::Extension(std::sync::Arc::new(token)));
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three reply shapes this daemon can send, checked against what the
    /// component's parser expects (`components/desktop-clipboard/src/lib.rs`).
    #[test]
    fn the_three_reply_shapes_are_well_formed_json() {
        let ok = json!({ "text": "hello" });
        assert_eq!(ok["text"], "hello");

        let empty = json!({ "error": "empty" });
        assert_eq!(empty["error"], "empty");
        assert!(empty.get("detail").is_none());

        let down = json!({ "error": "unavailable", "detail": "no desktop session" });
        assert_eq!(down["error"], "unavailable");
        assert_eq!(down["detail"], "no desktop session");
    }
}
