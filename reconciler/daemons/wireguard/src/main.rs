//! Native side of components/vpn-wireguard (ADR-0095). `wg-error::not-permitted` is never built:
//! `status` takes no interface name to refuse, it reads every allowed one.

use anyhow::Result;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use clap::Parser;
use serde_json::{json, Value};
use tokio::process::Command;

#[derive(Parser)]
#[command(
    name = "holon-wireguard",
    about = "Report WireGuard tunnel status, for a component that cannot look."
)]
struct Args {
    /// Shared secret callers must send as `Authorization: Bearer <token>`; unset means no check.
    #[arg(long)]
    token: Option<String>,
    /// Like `--token` but read from a file, since a flag is `ps`-readable; takes precedence.
    #[arg(long)]
    token_file: Option<std::path::PathBuf>,

    /// Where to listen; loopback by default, since this hands out network topology.
    #[arg(long, default_value = "127.0.0.1:8011")]
    addr: String,

    /// An interface this may read, repeatable; none means nothing is permitted.
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

/// The first line of `wg show <iface> dump` is the interface itself, not a peer.
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
        // No interface configured is a valid state meaning zero peers, not a failure.
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
    let token = comp_daemon_kit::resolve_token(args.token.clone(), args.token_file.clone());
    comp_daemon_kit::warn_if_unauthenticated("holon-wireguard", &token);
    if args.allow_interface.is_empty() {
        eprintln!(
            "holon-wireguard: no --allow-interface given, so status will report zero peers. \
             That is deliberate — an interface nobody has scoped reports nothing."
        );
    }
    println!(
        "holon-wireguard: listening on http://{} | {} allowed interface(s)",
        args.addr,
        args.allow_interface.len()
    );
    let state = std::sync::Arc::new(Daemon { allowed: args.allow_interface });
    let app = Router::new()
        .route("/status", post(status))
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

    #[test]
    fn an_interface_with_no_peers_parses_to_an_empty_list() {
        let dump = "privkey\tpubkey\t51820\t0\n";
        assert!(parse_dump(dump).is_empty());
    }

    #[test]
    fn no_allowed_interfaces_means_no_peers_not_an_error() {
        let d = Daemon { allowed: vec![] };
        assert!(d.allowed.is_empty());
    }
}
