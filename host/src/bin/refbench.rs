//! Native reference for the bench:suite ladder — the same /ok, /json, /echo
//! endpoints as `components/bench-suite`, but as a plain hyper server: no
//! wasm, no wasmCloud. The delta between this and the wasm suite on the same
//! machine IS the runtime/host tax. (No kv/blob rungs here — those would
//! measure a NATS client, not the framework.)
//!
//! Run: cargo run --release --bin refbench -- 127.0.0.1:3020

use std::net::SocketAddr;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

#[derive(Serialize, Deserialize)]
struct Pet {
    name: String,
    species: String,
    age: u32,
}

async fn handle(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let (parts, body) = req.into_parts();
    let resp = match (&parts.method, parts.uri.path()) {
        (&Method::GET, "/ok") => plain(StatusCode::OK, Bytes::new()),
        (&Method::GET, "/json") => {
            let pet = Pet { name: "Rex".into(), species: "dog".into(), age: 3 };
            json(StatusCode::OK, serde_json::to_vec(&pet).unwrap())
        }
        (&Method::POST, "/echo") => {
            let bytes = body.collect().await?.to_bytes();
            match serde_json::from_slice::<Pet>(&bytes) {
                Ok(pet) => json(StatusCode::OK, serde_json::to_vec(&pet).unwrap()),
                Err(_) => plain(StatusCode::BAD_REQUEST, Bytes::from_static(b"bad json")),
            }
        }
        _ => plain(StatusCode::NOT_FOUND, Bytes::from_static(b"not found")),
    };
    Ok(resp)
}

fn plain(status: StatusCode, body: Bytes) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain")
        .body(Full::new(body))
        .unwrap()
}

fn json(status: StatusCode, body: Vec<u8>) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:3020".to_string())
        .parse()?;
    let listener = TcpListener::bind(addr).await?;
    println!("refbench listening on {addr}");
    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            let _ = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service_fn(handle))
                .await;
        });
    }
}
