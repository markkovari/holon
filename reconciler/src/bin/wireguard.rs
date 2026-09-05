//! `comp-wireguard` — native daemon for vpn-wireguard
use anyhow::Result;
use axum::{routing::post, Json, Router};
use clap::Parser;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-wireguard", about = "Native daemon for vpn-wireguard")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8011")]
    addr: String,
}

async fn handle() -> Json<Value> {
    Json(json!({ "error": "UNIMPLEMENTED: vpn-wireguard cannot inspect a tunnel from wasm" }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    println!("comp-wireguard: listening on http://{}", args.addr);
    let app = Router::new().route("/call", post(handle));
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
