//! `comp-browser` — native daemon for browser-automation
use anyhow::Result;
use axum::{routing::post, Json, Router};
use clap::Parser;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-browser", about = "Native daemon for browser-automation")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8001")]
    addr: String,
}

async fn handle() -> Json<Value> {
    Json(json!({ "error": "UNIMPLEMENTED: browser-automation cannot drive a browser from wasm" }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    println!("comp-browser: listening on http://{}", args.addr);
    let app = Router::new().route("/call", post(handle));
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
