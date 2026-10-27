//! Native side of components/ui-notifier (ADR-0095); headless boxes have no notification bus.

use anyhow::Result;
use axum::routing::post;
use axum::{Json, Router};
use clap::Parser;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "holon-uinotify", about = "Native daemon for ui-notifier")]
struct Args {
    /// Shared secret callers must send as `Authorization: Bearer <token>`; unset means no check.
    #[arg(long)]
    token: Option<String>,
    /// Like `--token` but read from a file, since a flag is `ps`-readable; takes precedence.
    #[arg(long)]
    token_file: Option<std::path::PathBuf>,

    /// Where to listen; loopback by default.
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
    let token = comp_daemon_kit::resolve_token(args.token.clone(), args.token_file.clone());
    comp_daemon_kit::warn_if_unauthenticated("holon-uinotify", &token);
    println!("holon-uinotify: listening on http://{}", args.addr);
    let app = Router::new()
        .route("/notify", post(handle))
        .layer(axum::middleware::from_fn(comp_daemon_kit::require_token))
        .layer(axum::Extension(std::sync::Arc::new(token)));
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quote or backslash in the message must survive the serde round-trip.
    #[test]
    fn a_message_with_quotes_round_trips() {
        let body = r#"{"msg":"say \"hi\""}"#;
        let req: NotifyReq = serde_json::from_str(body).expect("valid json");
        assert_eq!(req.msg, r#"say "hi""#);
    }
}
