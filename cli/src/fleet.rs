//! The lattice lane: three control processes on real boxes, rendered from one spec.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{bail, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fleet {
    /// The isolation boundary: two lattices on one NATS never see each other's commands.
    pub lattice: String,
    pub nats_url: String,
    pub platform_url: String,

    pub nodes: Vec<Node>,

    /// Named, not defaulted: the only process that faces the network.
    pub ingress: Ingress,

    /// More than one is a standby: they contend for a lease (ADR-0072).
    #[serde(default)]
    pub reconcilers: Vec<Reconciler>,

    #[serde(default)]
    pub interval: Option<u64>,
    /// The failover detection time (ADR-0035).
    #[serde(default)]
    pub inventory_ttl: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    /// Must be stable across restarts: inventory is keyed by it.
    pub name: String,
    /// Not loopback: the ingress dials it from another box.
    pub addr: String,
    /// `sqlite` and `memory` are node-local, so a fleet wants `nats` (ADR-0027).
    #[serde(default = "default_node_kv")]
    pub kv: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ingress {
    /// A host to `scp` to, not a node.
    pub host: String,
    /// Loopback by default: a Caddy in front terminates TLS, not the ingress.
    #[serde(default = "default_ingress_addr")]
    pub addr: String,
    /// Requests in flight to one node before shedding with 503; 0 disables (ADR-0041).
    #[serde(default)]
    pub max_inflight: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reconciler {
    pub host: String,
    /// Must exceed `interval`: failover takes up to this plus one interval.
    #[serde(default)]
    pub lease_ttl: Option<u64>,
}

fn default_node_kv() -> String {
    // A node-local store under a fleet is what ADR-0027 refuses.
    "nats".into()
}
fn default_ingress_addr() -> String {
    // Caddy terminates TLS in front; the ingress speaks plain HTTP.
    "127.0.0.1:8088".into()
}

pub struct FleetLayout {
    pub bin_dir: PathBuf,
    pub env_dir: PathBuf,
}

impl Default for FleetLayout {
    fn default() -> Self {
        FleetLayout {
            bin_dir: PathBuf::from("/usr/local/bin"),
            env_dir: PathBuf::from("/etc/comp"),
        }
    }
}

pub fn check(f: &Fleet) -> Result<()> {
    if !is_label(&f.lattice) {
        bail!("lattice {:?} must be a lowercase DNS label", f.lattice);
    }
    for url in [&f.nats_url, &f.platform_url] {
        if url.trim().is_empty() || url.contains(char::is_whitespace) {
            bail!("{url:?} is not a URL");
        }
    }
    if !f.nats_url.starts_with("nats://") {
        bail!("nats_url {:?} must start with nats://", f.nats_url);
    }
    if f.nodes.is_empty() {
        bail!("a fleet with no nodes has nothing to run an app on");
    }

    let mut names = BTreeSet::new();
    let mut addrs = BTreeSet::new();
    for n in &f.nodes {
        if !is_label(&n.name) {
            bail!("node name {:?} must be a lowercase DNS label", n.name);
        }
        if !names.insert(n.name.clone()) {
            bail!("two nodes are both called {:?} — inventory is keyed by name", n.name);
        }
        if !addrs.insert(n.addr.clone()) {
            bail!("two nodes both advertise {:?}", n.addr);
        }
        if !matches!(n.kv.as_str(), "memory" | "sqlite" | "redis" | "nats") {
            bail!("node {}: kv must be memory|sqlite|redis|nats, got {:?}", n.name, n.kv);
        }
        // Loopback deploys clean but fails at the first request from another box.
        if n.addr.starts_with("127.") || n.addr.starts_with("localhost:") {
            bail!(
                "node {} advertises {:?}, which no other box can reach — advertise the address an ingress can dial",
                n.name, n.addr
            );
        }
    }

    // A lease shorter than a pass expires before renewal, so leadership would flap.
    let interval = f.interval.unwrap_or(10);
    for r in &f.reconcilers {
        if let Some(ttl) = r.lease_ttl {
            if ttl <= interval {
                bail!(
                    "reconciler on {}: lease_ttl {ttl} is not longer than interval {interval}, so the lease expires between renewals",
                    r.host
                );
            }
        }
    }
    Ok(())
}

fn is_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.starts_with(|c: char| c.is_ascii_alphanumeric())
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn hardening() -> &'static str {
    "Restart=always\n\
     RestartSec=2\n\
     DynamicUser=yes\n\
     NoNewPrivileges=yes\n\
     PrivateTmp=yes\n\
     PrivateDevices=yes\n\
     ProtectSystem=strict\n\
     ProtectHome=yes\n\
     ProtectKernelTunables=yes\n\
     ProtectControlGroups=yes\n\
     RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX\n\
     RestrictNamespaces=yes\n\
     LockPersonality=yes\n"
}

const GENERATED: &str =
    "# Generated by `holon fleet render` — do not edit; edit the fleet spec and re-deploy.\n";

pub fn render_node_unit(f: &Fleet, n: &Node, l: &FleetLayout) -> String {
    let mut s = String::from(GENERATED);
    s.push_str(&format!(
        "[Unit]\nDescription=comp-host: lattice node {} ({})\n\
         After=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\n",
        n.name, f.lattice
    ));
    s.push_str(&format!(
        "ExecStart={}/comp-host --lattice-nats {} --lattice {} --node {} --addr {} --kv {}",
        l.bin_dir.display(),
        f.nats_url,
        f.lattice,
        n.name,
        n.addr,
        n.kv
    ));
    // No --state-dir: comp-host defaults to $STATE_DIRECTORY, and systemd does not expand
    // ${STATE_DIRECTORY} in ExecStart, so the flag would arrive as that literal string.
    s.push('\n');
    s.push_str(&format!("StateDirectory=comp/node-{}\n", n.name));
    s.push_str(hardening());
    s.push_str("# wasmtime JITs, so it needs W^X — this one cannot be tightened.\nMemoryDenyWriteExecute=no\n");
    s.push_str("\n[Install]\nWantedBy=multi-user.target\n");
    s
}

pub fn render_reconciler_unit(f: &Fleet, r: &Reconciler, l: &FleetLayout) -> String {
    let mut s = String::from(GENERATED);
    s.push_str(&format!(
        "[Unit]\nDescription=comp-reconciler: {} control loop\n\
         After=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\n",
        f.lattice
    ));
    s.push_str(&format!(
        "ExecStart={}/comp-reconciler --platform-url {} --nats-url {} --lattice {}",
        l.bin_dir.display(),
        f.platform_url,
        f.nats_url,
        f.lattice
    ));
    if let Some(i) = f.interval {
        s.push_str(&format!(" --interval {i}"));
    }
    if let Some(t) = r.lease_ttl {
        s.push_str(&format!(" --lease-ttl {t}"));
    }
    s.push('\n');
    // Env file, never argv: any process on the box can read argv from /proc.
    s.push_str(&format!("EnvironmentFile={}/reconciler.env\n", l.env_dir.display()));
    s.push_str(hardening());
    s.push_str("\n[Install]\nWantedBy=multi-user.target\n");
    s
}

pub fn render_ingress_unit(f: &Fleet, l: &FleetLayout) -> String {
    let mut s = String::from(GENERATED);
    s.push_str(&format!(
        "[Unit]\nDescription=comp-ingress: {} door\n\
         After=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\n",
        f.lattice
    ));
    s.push_str(&format!(
        "ExecStart={}/comp-ingress --addr {} --nats-url {} --lattice {}",
        l.bin_dir.display(),
        f.ingress.addr,
        f.nats_url,
        f.lattice
    ));
    if let Some(m) = f.ingress.max_inflight {
        s.push_str(&format!(" --max-inflight {m}"));
    }
    if let Some(t) = f.inventory_ttl {
        s.push_str(&format!(" --inventory-ttl {t}"));
    }
    s.push('\n');
    s.push_str(hardening());
    s.push_str("\n[Install]\nWantedBy=multi-user.target\n");
    s
}

pub fn render_reconciler_env() -> String {
    let mut s = String::from(GENERATED);
    s.push_str(
        "# The platform secret, presented as `x-platform-secret`. Installed 0600 and\n\
         # read by systemd as root BEFORE privileges are dropped, so the reconciler's\n\
         # transient uid never has a path to it on disk.\n\
         #\n\
         # Fill this in on the box. It is deliberately not rendered from the spec: a\n\
         # secret that lives in a file you commit is not a secret (ADR-0010).\n\
         PLATFORM_SECRET=\n",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
lattice = "prod"
nats_url = "nats://10.0.0.1:4222"
platform_url = "http://10.0.0.1:8080"
[[nodes]]
name = "node-1"
addr = "10.0.0.2:3401"
[ingress]
host = "edge"
"#;

    fn fleet(t: &str) -> Fleet {
        toml::from_str(t).unwrap()
    }

    /// Prepended: in TOML every key after a `[table]` header belongs to that table.
    fn tuned() -> String {
        format!("interval = 3\ninventory_ttl = 15\n{MINIMAL}")
    }

    #[test]
    fn a_minimal_fleet_is_four_facts() {
        let f = fleet(MINIMAL);
        check(&f).unwrap();
        assert_eq!(f.nodes.len(), 1);
        assert_eq!(f.nodes[0].kv, "nats");
        assert_eq!(f.ingress.addr, "127.0.0.1:8088");
    }

    #[test]
    fn a_node_advertising_loopback_is_refused() {
        let f = fleet(
            "lattice = \"p\"\nnats_url = \"nats://a:4222\"\nplatform_url = \"http://a:8080\"\n\
             [[nodes]]\nname = \"n1\"\naddr = \"127.0.0.1:3401\"\n[ingress]\nhost = \"e\"\n",
        );
        let err = check(&f).unwrap_err().to_string();
        assert!(err.contains("no other box can reach"), "{err}");
    }

    #[test]
    fn two_nodes_cannot_share_a_name_because_inventory_is_keyed_by_it() {
        let f = fleet(
            "lattice = \"p\"\nnats_url = \"nats://a:4222\"\nplatform_url = \"http://a:8080\"\n\
             [[nodes]]\nname = \"n1\"\naddr = \"10.0.0.2:3401\"\n\
             [[nodes]]\nname = \"n1\"\naddr = \"10.0.0.3:3401\"\n[ingress]\nhost = \"e\"\n",
        );
        assert!(check(&f).unwrap_err().to_string().contains("both called"));
    }

    #[test]
    fn a_lease_shorter_than_a_pass_would_flap_forever() {
        let f = fleet(
            "lattice = \"p\"\nnats_url = \"nats://a:4222\"\nplatform_url = \"http://a:8080\"\n\
             interval = 10\n[[nodes]]\nname = \"n1\"\naddr = \"10.0.0.2:3401\"\n\
             [ingress]\nhost = \"e\"\n[[reconcilers]]\nhost = \"c1\"\nlease_ttl = 5\n",
        );
        let err = check(&f).unwrap_err().to_string();
        assert!(err.contains("expires between renewals"), "{err}");
    }

    #[test]
    fn two_reconcilers_are_a_standby_not_an_error() {
        let f = fleet(
            "lattice = \"p\"\nnats_url = \"nats://a:4222\"\nplatform_url = \"http://a:8080\"\n\
             [[nodes]]\nname = \"n1\"\naddr = \"10.0.0.2:3401\"\n[ingress]\nhost = \"e\"\n\
             [[reconcilers]]\nhost = \"c1\"\n[[reconcilers]]\nhost = \"c2\"\n",
        );
        check(&f).unwrap();
        assert_eq!(f.reconcilers.len(), 2);
    }

    #[test]
    fn the_node_unit_joins_the_lattice_and_is_hardened() {
        let f = fleet(MINIMAL);
        let u = render_node_unit(&f, &f.nodes[0], &FleetLayout::default());
        assert!(u.contains("--lattice-nats nats://10.0.0.1:4222"), "{u}");
        assert!(u.contains("--node node-1"), "{u}");
        assert!(u.contains("--addr 10.0.0.2:3401"), "{u}");
        assert!(u.contains("DynamicUser=yes"), "{u}");
        assert!(u.contains("ProtectSystem=strict"), "{u}");
        assert!(u.contains("MemoryDenyWriteExecute=no"), "{u}");
        assert!(!u.contains("--state-dir"), "comp-host defaults to $STATE_DIRECTORY: {u}");
        assert!(u.contains("StateDirectory=comp/node-node-1"), "{u}");
    }

    #[test]
    fn the_secret_reaches_the_reconciler_by_file_and_never_by_argv() {
        let f = fleet(MINIMAL);
        let r = Reconciler { host: "c1".into(), lease_ttl: None };
        let u = render_reconciler_unit(&f, &r, &FleetLayout::default());
        assert!(u.contains("EnvironmentFile=/etc/comp/reconciler.env"), "{u}");
        assert!(!u.contains("--secret"), "the secret must not be on the command line: {u}");
        assert!(render_reconciler_env().contains("PLATFORM_SECRET=\n"));
    }

    #[test]
    fn the_ingress_is_the_only_thing_that_faces_a_network() {
        let f = fleet(MINIMAL);
        let u = render_ingress_unit(&f, &FleetLayout::default());
        assert!(u.contains("--addr 127.0.0.1:8088"), "{u}");
        assert!(u.contains("--lattice prod"), "{u}");
    }

    #[test]
    fn the_tunables_reach_the_binaries_that_read_them() {
        let f = fleet(&tuned());
        let r = Reconciler { host: "c1".into(), lease_ttl: Some(30) };
        let ru = render_reconciler_unit(&f, &r, &FleetLayout::default());
        assert!(ru.contains("--interval 3"), "{ru}");
        assert!(ru.contains("--lease-ttl 30"), "{ru}");
        let iu = render_ingress_unit(&f, &FleetLayout::default());
        assert!(iu.contains("--inventory-ttl 15"), "{iu}");
    }

    #[test]
    fn every_flag_we_emit_exists_on_its_binary() {
        let f = fleet(&tuned());
        let r = Reconciler { host: "c1".into(), lease_ttl: Some(30) };
        let l = FleetLayout::default();
        for (bin, unit) in [
            ("comp-host", render_node_unit(&f, &f.nodes[0], &l)),
            ("comp-reconciler", render_reconciler_unit(&f, &r, &l)),
            ("comp-ingress", render_ingress_unit(&f, &l)),
        ] {
            let help = match help_for(bin) {
                Some(h) => h,
                // CI builds the release binaries before `cargo test`, so this only skips locally.
                None => {
                    eprintln!(
                        "skipping {bin}: not built — `cargo build --release` in its workspace"
                    );
                    continue;
                }
            };
            for flag in unit
                .lines()
                .find(|l| l.starts_with("ExecStart="))
                .unwrap()
                .split_whitespace()
                .filter(|w| w.starts_with("--"))
            {
                assert!(help.contains(flag), "{bin} has no {flag}:\n{unit}");
            }
        }
    }

    fn help_for(bin: &str) -> Option<String> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent()?;
        let path = root.join("target/release").join(bin);
        if !path.exists() {
            return None;
        }
        let out = std::process::Command::new(&path).arg("--help").output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}
