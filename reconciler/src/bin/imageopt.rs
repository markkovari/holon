//! `comp-imageopt` — native daemon for image-optimizer
use anyhow::Result;
use axum::{routing::post, Json, Router};
use clap::Parser;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-imageopt", about = "Native daemon for image-optimizer")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8004")]
    addr: String,
}

async fn handle() -> Json<Value> {
    Json(json!({ "error": "UNIMPLEMENTED: image-optimizer cannot re-encode an image from wasm" }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    println!("comp-imageopt: listening on http://{}", args.addr);
    let app = Router::new().route("/call", post(handle));
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
