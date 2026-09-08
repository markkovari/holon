//! `comp-llmlocal` — a prompt in, a completion out, for a component that cannot run a model.
//!
//! ## Why this is a process and not a component
//!
//! Running a model needs a runtime and weights on disk, and a `wasm32-wasip2`
//! guest has neither (ADR-0095). `components/llm-local` is the component
//! side: it holds the WIT contract and dials this over HTTP exactly like the
//! gate, the database and the model provider are reached.
//!
//! ## What "native" buys here, honestly
//!
//! This daemon does not run a model either. It proxies to a locally-running
//! **Ollama** server — the simplest real local-LLM backend to integrate
//! without a heavy native inference dependency of our own. That is the
//! boundary: no bundled model weights, no llama.cpp binding, no GPU code.
//! `--ollama-url` must point at an Ollama (or API-compatible) server already
//! running on the machine, or every request answers `unavailable`.
//!
//!   comp-llmlocal --addr 127.0.0.1:8006 --ollama-url http://127.0.0.1:11434 --model llama3.2

use anyhow::Result;
use axum::{extract::State, routing::post, Json, Router};
use clap::Parser;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-llmlocal", about = "Proxy a prompt to a local Ollama server.")]
struct Args {
    /// Where to listen. Loopback by default: this has no authentication of
    /// its own and is meant to be reached only from the sandbox host.
    #[arg(long, default_value = "127.0.0.1:8006")]
    addr: String,

    /// The Ollama (or API-compatible) server to proxy to.
    #[arg(long, default_value = "http://127.0.0.1:11434")]
    ollama_url: String,

    /// Which model Ollama should run the prompt through.
    #[arg(long, default_value = "llama3.2")]
    model: String,
}

struct Daemon {
    http: reqwest::Client,
    generate_url: String,
    model: String,
}

#[derive(Deserialize)]
struct InferReq {
    prompt: String,
}

/// The body Ollama's `/api/generate` wants, with `stream: false` so the reply
/// is one JSON object rather than a stream of partial ones — this daemon has
/// no caller that could consume a stream.
fn ollama_request_body(model: &str, prompt: &str) -> Value {
    json!({ "model": model, "prompt": prompt, "stream": false })
}

/// Ollama's non-streaming `/api/generate` reply is a JSON object with a
/// `response` field carrying the whole completion. Anything else — a
/// different shape, a missing field — is treated as no answer rather than
/// guessed at.
fn ollama_response_text(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    v.get("response")?.as_str().map(str::to_string)
}

async fn infer(State(d): State<std::sync::Arc<Daemon>>, Json(req): Json<InferReq>) -> Json<Value> {
    let body = ollama_request_body(&d.model, &req.prompt);
    let resp = match d.http.post(&d.generate_url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => return Json(json!({ "error": "unavailable", "detail": e.to_string() })),
    };
    if !resp.status().is_success() {
        let status = resp.status();
        return Json(json!({ "error": "unavailable", "detail": format!("ollama returned {status}") }));
    }
    let text = match resp.text().await {
        Ok(t) => t,
        Err(e) => return Json(json!({ "error": "unavailable", "detail": e.to_string() })),
    };
    match ollama_response_text(&text) {
        Some(response) => Json(json!({ "response": response })),
        None => Json(json!({ "error": "unavailable", "detail": "ollama reply had no response field" })),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    println!(
        "comp-llmlocal: listening on http://{} | ollama at {} | model {}",
        args.addr, args.ollama_url, args.model
    );
    let state = std::sync::Arc::new(Daemon {
        http: reqwest::Client::new(),
        generate_url: format!("{}/api/generate", args.ollama_url.trim_end_matches('/')),
        model: args.model,
    });
    let app = Router::new().route("/infer", post(infer)).with_state(state);
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_body_carries_the_model_and_prompt_without_streaming() {
        let body = ollama_request_body("llama3.2", "hello");
        assert_eq!(body["model"], "llama3.2");
        assert_eq!(body["prompt"], "hello");
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn a_response_field_is_read_out_of_ollamas_reply() {
        let body = r#"{"model":"llama3.2","response":"hi there","done":true}"#;
        assert_eq!(ollama_response_text(body), Some("hi there".to_string()));
    }

    #[test]
    fn a_reply_with_no_response_field_is_no_answer() {
        assert_eq!(ollama_response_text(r#"{"done":true}"#), None);
        assert_eq!(ollama_response_text("not json"), None);
    }
}
