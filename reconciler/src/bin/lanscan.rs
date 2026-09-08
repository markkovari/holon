//! `comp-lanscan` — which hosts on the local network answer, for a component that cannot look.
//!
//! ## Why this is a process and not a component
//!
//! Scanning a LAN means opening many outbound TCP connections against
//! addresses a caller does not supply, and a `wasm32-wasip2` guest has no way
//! to do that. That is the sandbox working rather than a gap to route around
//! (ADR-0095), so the part that needs an operating system is native and is
//! reached over HTTP exactly like the gate, the database and the model
//! provider are.
//!
//! `components/lan-scanner` is the component side: it holds the WIT contract
//! and dials this. Nothing here knows what a goal is.
//!
//! ## An allow-list, for the same reason `comp-fswatch` has one
//!
//! A scan's scope is not something a caller may set — the request carries no
//! target at all. `--allow-cidr 192.168.1.0/24` permits scanning that
//! network; nothing is permitted by default, which is the correct behaviour
//! for a capability nobody has scoped yet.
//!
//! A CIDR broader than /24 is refused rather than scanned, to keep a scan
//! bounded: a /24 is at most 254 hosts, and a /16 is 65534.
//!
//! ## No raw ICMP
//!
//! A true ping needs a raw socket, which needs root this daemon does not
//! assume it has. Reachability here is a TCP-connect probe against
//! `--port` (default 80): a successful connect means "something is
//! listening", a refused or timed-out one means "unreachable", and neither is
//! a scan failure — only "no --allow-cidr configured" or a CIDR that fails to
//! parse is.
//!
//!   comp-lanscan --addr 127.0.0.1:8005 --allow-cidr 192.168.1.0/24 --port 80 --timeout-ms 200

use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::Result;
use axum::{extract::State, routing::post, Json, Router};
use clap::Parser;
use serde_json::{json, Value};
use tokio::net::TcpStream;

#[derive(Parser)]
#[command(name = "comp-lanscan", about = "Report which LAN hosts answer, for a component that cannot look.")]
struct Args {
    /// Where to listen. Loopback by default: this hands out a picture of the
    /// local network and has no authentication of its own.
    #[arg(long, default_value = "127.0.0.1:8005")]
    addr: String,

    /// A CIDR this may scan, repeatable. Empty means nothing is permitted,
    /// which is what a capability nobody has scoped should do. Anything
    /// broader than /24 is refused rather than silently truncated.
    #[arg(long = "allow-cidr")]
    allow_cidr: Vec<String>,

    /// The TCP port probed on every host to decide reachability.
    #[arg(long, default_value_t = 80)]
    port: u16,

    /// How long to wait for a connect before calling a host unreachable.
    #[arg(long = "timeout-ms", default_value_t = 200)]
    timeout_ms: u64,
}

/// Every host address in `cidr`, or `None` if it does not parse or is broader
/// than /24 (kept out of scope so a scan stays bounded — at most 254 hosts).
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
    let reachable = tokio::time::timeout(timeout, TcpStream::connect((ip, port))).await.is_ok_and(|r| r.is_ok());
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
            None => eprintln!("comp-lanscan: ignoring {cidr:?} — not a parseable CIDR of /24 or narrower"),
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
    let state = std::sync::Arc::new(Daemon { hosts, port: args.port, timeout: Duration::from_millis(args.timeout_ms) });
    let app = Router::new().route("/scan", post(scan)).with_state(state);
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A /24 enumerates its 254 usable-plus-broadcast-plus-network addresses
    /// (256 total, this function does not special-case network/broadcast —
    /// they get probed too and will simply read as unreachable).
    #[test]
    fn a_slash_24_enumerates_its_256_addresses() {
        let hosts = hosts_in_cidr("192.168.1.0/24").expect("parses");
        assert_eq!(hosts.len(), 256);
        assert_eq!(hosts[0], Ipv4Addr::new(192, 168, 1, 0));
        assert_eq!(hosts[255], Ipv4Addr::new(192, 168, 1, 255));
    }

    /// A /32 is a single host.
    #[test]
    fn a_slash_32_is_a_single_host() {
        let hosts = hosts_in_cidr("10.0.0.5/32").expect("parses");
        assert_eq!(hosts, vec![Ipv4Addr::new(10, 0, 0, 5)]);
    }

    /// Anything broader than /24 is refused rather than scanned, to keep a
    /// scan bounded.
    #[test]
    fn a_cidr_broader_than_slash_24_is_refused() {
        assert!(hosts_in_cidr("10.0.0.0/16").is_none());
        assert!(hosts_in_cidr("not-a-cidr").is_none());
        assert!(hosts_in_cidr("10.0.0.0").is_none());
    }
}
