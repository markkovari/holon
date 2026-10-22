//! Native side of components/desktop-clipboard (ADR-0095): a wasm guest has no pasteboard.

use anyhow::Result;
use axum::routing::post;
use axum::{Json, Router};
use clap::Parser;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-clipboard", about = "Native daemon for desktop-clipboard")]
struct Args {
    /// Shared secret callers must send as `Authorization: Bearer <token>`; unset means no check.
    #[arg(long)]
    token: Option<String>,
    /// Like `--token` but read from a file, since a flag is `ps`-readable; takes precedence.
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
        // An image, or nothing: either way this text contract has nothing to return.
        Err(arboard::Error::ContentNotAvailable) => Json(json!({ "error": "empty" })),
        Err(e) => Json(json!({ "error": "unavailable", "detail": e.to_string() })),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token = comp_daemon_kit::resolve_token(args.token.clone(), args.token_file.clone());
    comp_daemon_kit::warn_if_unauthenticated("comp-clipboard", &token);
    println!("comp-clipboard: listening on http://{}", args.addr);
    let app = Router::new()
        .route("/call", post(handle))
        .layer(axum::middleware::from_fn(comp_daemon_kit::require_token))
        .layer(axum::Extension(std::sync::Arc::new(token)));
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Must match the parser in components/desktop-clipboard/src/lib.rs.
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
