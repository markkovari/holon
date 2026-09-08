//! `comp-docker` — native daemon for container-docker
//!
//! Listing containers needs the docker socket, and a `wasm32-wasip2` guest
//! has none of those (ADR-0095). `components/container-docker` is the
//! component side: it holds the WIT contract and dials this over HTTP.
//! Nothing here knows what a goal is.
//!
//! ## What "ps" means here
//!
//! `bollard::Docker::list_containers(None)` — no filters — lists only
//! running containers, matching plain `docker ps` without `-a`. A stopped
//! container is not reported. That is deliberate rather than an oversight:
//! this component's contract is about what is running, and a caller wanting
//! everything can ask for a different capability.
//!
//! Any connection failure — the docker daemon not running, the socket not
//! found — comes back as `{"error":"unavailable","detail":"..."}` rather than
//! a crash: that is the honest answer for an environment with no docker.

use anyhow::Result;
use axum::{routing::post, Json, Router};
use bollard::models::ContainerSummary;
use clap::Parser;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-docker", about = "Native daemon for container-docker")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8002")]
    addr: String,
}

/// The short id `docker ps` shows: the first 12 characters of the full one.
/// A shorter id is returned as-is rather than padded — that only happens for
/// a test fixture or a very unusual engine, never a real docker daemon.
fn short_id(id: &str) -> &str {
    &id[..id.len().min(12)]
}

/// `bollard` gives each name prefixed with `/` (docker's own convention, left
/// over from a Swarm-mode addressing scheme). The first name, unprefixed, or
/// empty if the engine reported none.
fn primary_name(names: &[String]) -> &str {
    names.first().map(|n| n.strip_prefix('/').unwrap_or(n)).unwrap_or("")
}

fn container_json(c: &ContainerSummary) -> Value {
    let id = c.id.as_deref().unwrap_or("");
    let names = c.names.clone().unwrap_or_default();
    json!({
        "id": short_id(id),
        "image": c.image.as_deref().unwrap_or(""),
        "status": c.status.as_deref().unwrap_or(""),
        "name": primary_name(&names),
    })
}

async fn handle() -> Json<Value> {
    let client = match bollard::Docker::connect_with_local_defaults() {
        Ok(c) => c,
        Err(e) => return Json(json!({ "error": "unavailable", "detail": e.to_string() })),
    };
    match client.list_containers::<String>(None).await {
        Ok(containers) => {
            let list: Vec<Value> = containers.iter().map(container_json).collect();
            Json(json!({ "containers": list }))
        }
        Err(e) => Json(json!({ "error": "unavailable", "detail": e.to_string() })),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    println!("comp-docker: listening on http://{}", args.addr);
    let app = Router::new().route("/call", post(handle));
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_id_is_truncated_to_the_short_form() {
        assert_eq!(short_id("abc123def4567890abcdef"), "abc123def456");
    }

    #[test]
    fn a_short_id_is_left_alone() {
        assert_eq!(short_id("abc123"), "abc123");
    }

    #[test]
    fn the_leading_slash_is_stripped_from_the_first_name() {
        assert_eq!(primary_name(&["/web".to_string()]), "web");
        assert_eq!(primary_name(&["/web".to_string(), "/web-alias".to_string()]), "web");
    }

    #[test]
    fn no_names_is_reported_as_empty_rather_than_panicking() {
        assert_eq!(primary_name(&[]), "");
    }
}
