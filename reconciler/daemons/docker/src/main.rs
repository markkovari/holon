//! Native side of components/container-docker (ADR-0095); lists running containers only, like `docker ps`.

use anyhow::Result;
use axum::routing::post;
use axum::{Json, Router};
use bollard::models::ContainerSummary;
use clap::Parser;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "holon-docker", about = "Native daemon for container-docker")]
struct Args {
    /// Shared secret callers must send as `Authorization: Bearer <token>`; unset means no check.
    #[arg(long)]
    token: Option<String>,
    /// Like `--token` but read from a file, since a flag is `ps`-readable; takes precedence.
    #[arg(long)]
    token_file: Option<std::path::PathBuf>,

    #[arg(long, default_value = "127.0.0.1:8002")]
    addr: String,
}

fn short_id(id: &str) -> &str {
    &id[..id.len().min(12)]
}

/// bollard prefixes each name with `/` (a Swarm-era convention).
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
    let token = comp_daemon_kit::resolve_token(args.token.clone(), args.token_file.clone());
    comp_daemon_kit::warn_if_unauthenticated("holon-docker", &token);
    println!("holon-docker: listening on http://{}", args.addr);
    let app = Router::new()
        .route("/call", post(handle))
        .layer(axum::middleware::from_fn(comp_daemon_kit::require_token))
        .layer(axum::Extension(std::sync::Arc::new(token)));
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
