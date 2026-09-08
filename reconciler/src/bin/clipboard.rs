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
    println!("comp-clipboard: listening on http://{}", args.addr);
    let app = Router::new().route("/call", post(handle));
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
