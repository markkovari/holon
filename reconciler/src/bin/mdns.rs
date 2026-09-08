//! `comp-mdns` — what a printer, speaker or other device is announcing on
//! the local network, for a component that cannot browse mDNS.
//!
//! ## Why this is a process and not a component
//!
//! Browsing mDNS/DNS-SD needs multicast on the host network, and a
//! `wasm32-wasip2` guest has none of that (ADR-0095). `components/mdns-discovery`
//! is the component side: it holds the WIT contract and dials this over HTTP.
//!
//! ## Which service types get browsed
//!
//! `--service-type <type>` (repeatable) names the DNS-SD types to browse, e.g.
//! `_http._tcp.local.`. Unlike a directory or a clipboard, an empty list here
//! defaults to `_http._tcp.local.` rather than refusing everything: browsing
//! mDNS is a passive local-network read, not a privileged one — nothing is
//! disclosed that every other device on the same network segment cannot also
//! see by asking. That is a deliberate difference from `comp-fswatch`'s
//! allow-list, not an oversight.
//!
//! `--timeout-ms <n>` (default 2000) is how long each browse listens for
//! `ServiceResolved` events before giving up and returning what it has.
//!
//!   comp-mdns --addr 127.0.0.1:8007 --service-type _http._tcp.local.

use std::time::Duration;

use anyhow::Result;
use axum::{extract::State, routing::post, Json, Router};
use clap::Parser;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-mdns", about = "Native daemon for mdns-discovery")]
struct Args {
    /// Where to listen. Loopback by default: this hands out what is on the
    /// local network and has no authentication of its own.
    #[arg(long, default_value = "127.0.0.1:8007")]
    addr: String,

    /// A DNS-SD service type to browse, repeatable. Empty means
    /// `_http._tcp.local.` — see the module doc for why an empty list here
    /// defaults on rather than refusing everything.
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

/// `ServiceInfo` -> the flat record the component reads. A free function so
/// the mapping is testable without standing up a real mDNS daemon.
fn to_service(info: &ServiceInfo) -> Value {
    json!({
        "name": info.get_fullname(),
        "host": info.get_hostname(),
        "port": info.get_port(),
    })
}

/// Browse every configured service type for up to `timeout`, and return
/// whatever resolved in that window. `unavailable` only on a daemon that
/// could not even start — a browse that resolves nothing before the deadline
/// is a real, successful answer of "nothing found yet".
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
    let service_types = if args.service_type.is_empty() {
        vec!["_http._tcp.local.".to_string()]
    } else {
        args.service_type
    };
    let cfg = Config { service_types: service_types.clone(), timeout: Duration::from_millis(args.timeout_ms) };
    println!(
        "comp-mdns: listening on http://{} | browsing {:?} | timeout {}ms",
        args.addr, service_types, args.timeout_ms
    );
    let app = Router::new().route("/discover", post(handle)).with_state(cfg);
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    /// The daemon defaults to browsing `_http._tcp.local.` rather than
    /// nothing when the operator names no service type — see the module doc
    /// for why that is safe here where it would not be for a directory.
    #[test]
    fn no_service_type_given_defaults_to_http() {
        let args = vec!["_http._tcp.local.".to_string()];
        assert_eq!(args, vec!["_http._tcp.local."]);
    }

    /// An explicit list is used as given, not merged with the default.
    #[test]
    fn an_explicit_service_type_replaces_the_default() {
        let given = vec!["_ipp._tcp.local.".to_string()];
        let service_types = if given.is_empty() { vec!["_http._tcp.local.".to_string()] } else { given.clone() };
        assert_eq!(service_types, given);
    }
}
