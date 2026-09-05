//! `comp-mdns` — native daemon for mdns-discovery
use anyhow::Result;
use axum::{routing::post, Json, Router};
use clap::Parser;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-mdns", about = "Native daemon for mdns-discovery")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8007")]
    addr: String,
}

async fn handle() -> Json<Value> {
    Json(json!({ "error": "UNIMPLEMENTED: mdns-discovery cannot browse mDNS from wasm" }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    println!("comp-mdns: listening on http://{}", args.addr);
    let app = Router::new().route("/call", post(handle));
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
