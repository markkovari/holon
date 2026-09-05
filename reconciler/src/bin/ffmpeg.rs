//! `comp-ffmpeg` — native daemon for video-ffmpeg
use anyhow::Result;
use axum::{routing::post, Json, Router};
use clap::Parser;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-ffmpeg", about = "Native daemon for video-ffmpeg")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8010")]
    addr: String,
}

async fn handle() -> Json<Value> {
    Json(json!({ "error": "UNIMPLEMENTED: video-ffmpeg cannot transcode video from wasm" }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    println!("comp-ffmpeg: listening on http://{}", args.addr);
    let app = Router::new().route("/call", post(handle));
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
