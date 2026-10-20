//! Native side of components/lan-scanner (ADR-0095). Reachability is a TCP connect to `--port`: a
//! real ping needs a raw socket, and so root.

use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::Result;
use axum::{extract::State, routing::post, Json, Router};
use clap::Parser;
use serde_json::{json, Value};
use tokio::net::TcpStream;

#[derive(Parser)]
#[command(
    name = "comp-lanscan",
    about = "Report which LAN hosts answer, for a component that cannot look."
)]
struct Args {
    /// Shared secret callers must send as `Authorization: Bearer <token>`; unset means no check.
    #[arg(long)]
    token: Option<String>,
    /// Like `--token` but read from a file, since a flag is `ps`-readable; takes precedence.
    #[arg(long)]
    token_file: Option<std::path::PathBuf>,

    /// Where to listen; loopback by default, since this maps the local network.
    #[arg(long, default_value = "127.0.0.1:8005")]
    addr: String,

    /// A CIDR this may scan, repeatable; none permits nothing, broader than /24 is refused.
    #[arg(long = "allow-cidr")]
    allow_cidr: Vec<String>,

    /// The TCP port probed on every host to decide reachability.
    #[arg(long, default_value_t = 80)]
    port: u16,

    /// How long to wait for a connect before calling a host unreachable.
    #[arg(long = "timeout-ms", default_value_t = 200)]
    timeout_ms: u64,
}

/// `None` if unparseable or broader than /24, so a scan stays bounded.
fn hosts_in_cidr(cidr: &str) -> Option<Vec<Ipv4Addr>> {
    let (addr, prefix) = cidr.split_once('/')?;
    let base: Ipv4Addr = addr.parse().ok()?;
    let prefix: u32 = prefix.parse().ok()?;
    if !(24..=32).contains(&prefix) {
        return None;
    }
    let mask = if prefix == 32 { u32::MAX } else { !0u32 << (32 - prefix) };
    let network = u32::from(base) & mask;
    let host_bits = 32 - prefix;
    let count = 1u32 << host_bits;
    Some((0..count).map(|i| Ipv4Addr::from(network + i)).collect())
}

struct Daemon {
    hosts: Vec<Ipv4Addr>,
    port: u16,
    timeout: Duration,
}

async fn probe(ip: Ipv4Addr, port: u16, timeout: Duration) -> (Ipv4Addr, bool) {
    let reachable = tokio::time::timeout(timeout, TcpStream::connect((ip, port)))
        .await
        .is_ok_and(|r| r.is_ok());
    (ip, reachable)
}

async fn scan(State(d): State<std::sync::Arc<Daemon>>) -> Json<Value> {
    if d.hosts.is_empty() {
        return Json(json!({ "error": "unavailable", "detail": "no --allow-cidr configured" }));
    }

    let probes = d.hosts.iter().map(|&ip| probe(ip, d.port, d.timeout));
    let results = futures::future::join_all(probes).await;

    let out: Vec<Value> = results
        .into_iter()
        .map(|(ip, reachable)| json!({ "ip": ip.to_string(), "reachable": reachable }))
        .collect();
    Json(json!({ "hosts": out }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token = comp_daemon_kit::resolve_token(args.token.clone(), args.token_file.clone());
    comp_daemon_kit::warn_if_unauthenticated("comp-lanscan", &token);
    if args.allow_cidr.is_empty() {
        eprintln!(
            "comp-lanscan: no --allow-cidr given, so every scan will report unavailable. \
             That is deliberate — a scanner nobody has scoped scans nothing."
        );
    }
    let mut hosts = Vec::new();
    for cidr in &args.allow_cidr {
        match hosts_in_cidr(cidr) {
            Some(h) => hosts.extend(h),
            None => eprintln!(
                "comp-lanscan: ignoring {cidr:?} — not a parseable CIDR of /24 or narrower"
            ),
        }
    }
    println!(
        "comp-lanscan: listening on http://{} | {} CIDR(s), {} host(s) | port {} | timeout {}ms",
        args.addr,
        args.allow_cidr.len(),
        hosts.len(),
        args.port,
        args.timeout_ms
    );
    let state = std::sync::Arc::new(Daemon {
        hosts,
        port: args.port,
        timeout: Duration::from_millis(args.timeout_ms),
    });
    let app = Router::new()
        .route("/scan", post(scan))
        .with_state(state)
        .layer(axum::middleware::from_fn(comp_daemon_kit::require_token))
        .layer(axum::Extension(std::sync::Arc::new(token)));
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Network and broadcast are not special-cased: they are probed too and read as unreachable.
    #[test]
    fn a_slash_24_enumerates_its_256_addresses() {
        let hosts = hosts_in_cidr("192.168.1.0/24").expect("parses");
        assert_eq!(hosts.len(), 256);
        assert_eq!(hosts[0], Ipv4Addr::new(192, 168, 1, 0));
        assert_eq!(hosts[255], Ipv4Addr::new(192, 168, 1, 255));
    }

    #[test]
    fn a_slash_32_is_a_single_host() {
        let hosts = hosts_in_cidr("10.0.0.5/32").expect("parses");
        assert_eq!(hosts, vec![Ipv4Addr::new(10, 0, 0, 5)]);
    }

    #[test]
    fn a_cidr_broader_than_slash_24_is_refused() {
        assert!(hosts_in_cidr("10.0.0.0/16").is_none());
        assert!(hosts_in_cidr("not-a-cidr").is_none());
        assert!(hosts_in_cidr("10.0.0.0").is_none());
    }
}
