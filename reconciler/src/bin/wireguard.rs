//! `comp-wireguard` — whether a WireGuard tunnel is up, for a component that cannot look.
//!
//! ## Why this is a process and not a component
//!
//! Reading a WireGuard interface means running the `wg` binary and reading a
//! kernel module's state, neither of which a `wasm32-wasip2` guest can do.
//! That is the sandbox working rather than a gap to route around (ADR-0095),
//! so the part that needs an operating system is native and is reached over
//! HTTP exactly like the gate, the database and the model provider are.
//!
//! `components/vpn-wireguard` is the component side: it holds the WIT
//! contract and dials this. Nothing here knows what a goal is.
//!
//! ## An allow-list, for the same reason `comp-fswatch` has one
//!
//! `--allow-interface wg0` permits reading that interface, repeatable;
//! nothing is permitted by default. Unlike `comp-fswatch`, `status` takes no
//! argument — there is no caller-supplied interface name to refuse — so this
//! daemon just iterates every allowed interface itself and concatenates their
//! peers. `wg-error::not-permitted` therefore exists in the WIT for symmetry
//! with the other capabilities' allow-list shape, but this daemon never
//! constructs it: there is nothing for a caller to name that could be
//! refused.
//!
//! ## Parsing `wg show <iface> dump`
//!
//! The first line describes the interface itself (private-key, public-key,
//! listen-port, fwmark) and is not a peer; every subsequent line is
//! tab-separated: public-key, preshared-key, endpoint, allowed-ips,
//! latest-handshake, transfer-rx, transfer-tx, persistent-keepalive.
//!
//!   comp-wireguard --addr 127.0.0.1:8011 --allow-interface wg0

use anyhow::Result;
use axum::{extract::State, routing::post, Json, Router};
use clap::Parser;
use serde_json::{json, Value};
use tokio::process::Command;

#[derive(Parser)]
#[command(name = "comp-wireguard", about = "Report WireGuard tunnel status, for a component that cannot look.")]
struct Args {
    /// Shared secret a caller must send as `Authorization: Bearer
    /// <token>`. Loopback binding alone is not a boundary — see
    /// `comp_reconciler::daemon_auth`'s own doc for why. No token means
    /// no check, logged loudly rather than silently.
    #[arg(long)]
    token: Option<String>,
    /// Same, but read from a file (a systemd `LoadCredential` path)
    /// rather than passed as a value — `--token` is `ps`-readable by
    /// any local user, which is most of what this exists to close.
    /// Wins over `--token` when both are given.
    #[arg(long)]
    token_file: Option<std::path::PathBuf>,

    /// Where to listen. Loopback by default: this hands out network topology
    /// and has no authentication of its own.
    #[arg(long, default_value = "127.0.0.1:8011")]
    addr: String,

    /// An interface this may read, repeatable. Empty means nothing is
    /// permitted, which is what a capability nobody has scoped should do.
    #[arg(long = "allow-interface")]
    allow_interface: Vec<String>,
}

#[derive(Debug, PartialEq)]
struct Peer {
    public_key: String,
    endpoint: String,
    latest_handshake: u64,
    rx_bytes: u64,
    tx_bytes: u64,
}

/// Parse `wg show <iface> dump` output into its peer lines, skipping the
/// first line (the interface's own private-key/public-key/listen-port/fwmark
/// row, which is not a peer).
fn parse_dump(output: &str) -> Vec<Peer> {
    output
        .lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 7 {
                return None;
            }
            Some(Peer {
                public_key: f[0].to_string(),
                endpoint: f[2].to_string(),
                latest_handshake: f[4].parse().unwrap_or(0),
                rx_bytes: f[5].parse().unwrap_or(0),
                tx_bytes: f[6].parse().unwrap_or(0),
            })
        })
        .collect()
}

struct Daemon {
    allowed: Vec<String>,
}

async fn status(State(d): State<std::sync::Arc<Daemon>>) -> Json<Value> {
    if d.allowed.is_empty() {
        // No interface configured is a valid state meaning zero peers, not a
        // failure — the same reasoning as no crontab configured.
        return Json(json!({ "peers": [] }));
    }

    let mut peers = Vec::new();
    let mut ran_any = false;
    for iface in &d.allowed {
        match Command::new("wg").args(["show", iface, "dump"]).output().await {
            Ok(out) if out.status.success() => {
                ran_any = true;
                let text = String::from_utf8_lossy(&out.stdout);
                peers.extend(parse_dump(&text));
            }
            _ => {}
        }
    }

    if !ran_any {
        return Json(json!({
            "error": "unavailable",
            "detail": "the wg binary is missing, or every allowed interface failed"
        }));
    }

    let out: Vec<Value> = peers
        .into_iter()
        .map(|p| {
            json!({
                "public_key": p.public_key,
                "endpoint": p.endpoint,
                "latest_handshake": p.latest_handshake,
                "rx_bytes": p.rx_bytes,
                "tx_bytes": p.tx_bytes,
            })
        })
        .collect();
    Json(json!({ "peers": out }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token = comp_reconciler::daemon_auth::resolve_token(args.token.clone(), args.token_file.clone());
    comp_reconciler::daemon_auth::warn_if_unauthenticated("comp-wireguard", &token);
    if args.allow_interface.is_empty() {
        eprintln!(
            "comp-wireguard: no --allow-interface given, so status will report zero peers. \
             That is deliberate — an interface nobody has scoped reports nothing."
        );
    }
    println!(
        "comp-wireguard: listening on http://{} | {} allowed interface(s)",
        args.addr,
        args.allow_interface.len()
    );
    let state = std::sync::Arc::new(Daemon { allowed: args.allow_interface });
    let app = Router::new().route("/status", post(status)).with_state(state)
        .layer(axum::middleware::from_fn(comp_reconciler::daemon_auth::require_token))
        .layer(axum::Extension(std::sync::Arc::new(token)));
    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first line is the interface itself and must not be read as a peer;
    /// the rest parse into their five fields.
    #[test]
    fn a_dump_parses_its_peer_lines_and_skips_the_interface_line() {
        let dump = "privkey\tpubkey\t51820\t0\n\
                    peerkey1=\t(none)\t1.2.3.4:51820\t0.0.0.0/0\t1700000000\t1024\t2048\t25\n\
                    peerkey2=\t(none)\t5.6.7.8:51820\t10.0.0.0/24\t0\t0\t0\toff\n";
        let peers = parse_dump(dump);
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].public_key, "peerkey1=");
        assert_eq!(peers[0].endpoint, "1.2.3.4:51820");
        assert_eq!(peers[0].latest_handshake, 1700000000);
        assert_eq!(peers[0].rx_bytes, 1024);
        assert_eq!(peers[0].tx_bytes, 2048);
        assert_eq!(peers[1].public_key, "peerkey2=");
        assert_eq!(peers[1].latest_handshake, 0);
    }

    /// An interface with no peers dumps just its own line.
    #[test]
    fn an_interface_with_no_peers_parses_to_an_empty_list() {
        let dump = "privkey\tpubkey\t51820\t0\n";
        assert!(parse_dump(dump).is_empty());
    }

    /// No `--allow-interface` at all means zero peers, not an error — a valid
    /// state rather than a scan that failed.
    #[test]
    fn no_allowed_interfaces_means_no_peers_not_an_error() {
        let d = Daemon { allowed: vec![] };
        assert!(d.allowed.is_empty());
    }
}
