//! Native side of components/mdns-discovery (ADR-0095). No `--service-type` browses `_http._tcp.local.`
//! rather than refusing: mDNS is a passive read any device on the segment can do.

use std::time::Duration;

use anyhow::Result;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use clap::Parser;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-mdns", about = "Native daemon for mdns-discovery")]
struct Args {
    /// Shared secret callers must send as `Authorization: Bearer <token>`; unset means no check.
    #[arg(long)]
    token: Option<String>,
    /// Like `--token` but read from a file, since a flag is `ps`-readable; takes precedence.
    #[arg(long)]
    token_file: Option<std::path::PathBuf>,

    /// Where to listen; loopback by default, since this reveals what is on the local network.
    #[arg(long, default_value = "127.0.0.1:8007")]
    addr: String,

    /// A DNS-SD service type to browse, repeatable; none means `_http._tcp.local.`.
    #[arg(long = "service-type")]
    service_type: Vec<String>,

    /// How long to listen for responses to one browse, in milliseconds.
    #[arg(long = "timeout-ms", default_value_t = 2000)]
    timeout_ms: u64,
}

#[derive(Clone)]
struct Config {
    service_types: Vec<String>,
    timeout: Duration,
}

fn to_service(info: &ServiceInfo) -> Value {
    json!({
        "name": info.get_fullname(),
        "host": info.get_hostname(),
        "port": info.get_port(),
    })
}

/// Nothing resolved by the deadline is a real empty answer; `unavailable` means it could not start.
fn discover(cfg: &Config) -> Result<Vec<Value>, String> {
    let mdns = ServiceDaemon::new().map_err(|e| e.to_string())?;
    let mut found = Vec::new();

    for ty in &cfg.service_types {
        let Ok(receiver) = mdns.browse(ty) else { continue };
        let deadline = std::time::Instant::now() + cfg.timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match receiver.recv_timeout(remaining) {
                Ok(ServiceEvent::ServiceResolved(info)) => found.push(to_service(&info)),
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    }

    let _ = mdns.shutdown();
    Ok(found)
}

async fn handle(State(cfg): State<Config>) -> Json<Value> {
    match discover(&cfg) {
        Ok(services) => Json(json!({ "services": services })),
        Err(detail) => Json(json!({ "error": "unavailable", "detail": detail })),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token = comp_daemon_kit::resolve_token(args.token.clone(), args.token_file.clone());
    comp_daemon_kit::warn_if_unauthenticated("comp-mdns", &token);
    let service_types = if args.service_type.is_empty() {
        vec!["_http._tcp.local.".to_string()]
    } else {
        args.service_type
    };
    let cfg = Config {
        service_types: service_types.clone(),
        timeout: Duration::from_millis(args.timeout_ms),
    };
    println!(
        "comp-mdns: listening on http://{} | browsing {:?} | timeout {}ms",
        args.addr, service_types, args.timeout_ms
    );
    let app = Router::new()
        .route("/discover", post(handle))
        .with_state(cfg)
        .layer(axum::middleware::from_fn(comp_daemon_kit::require_token))
        .layer(axum::Extension(std::sync::Arc::new(token)));
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn no_service_type_given_defaults_to_http() {
        let args = vec!["_http._tcp.local.".to_string()];
        assert_eq!(args, vec!["_http._tcp.local."]);
    }

    #[test]
    fn an_explicit_service_type_replaces_the_default() {
        let given = vec!["_ipp._tcp.local.".to_string()];
        let service_types =
            if given.is_empty() { vec!["_http._tcp.local.".to_string()] } else { given.clone() };
        assert_eq!(service_types, given);
    }
}
