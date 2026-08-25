//! The lattice lane: three control processes on real boxes, rendered from one spec.
//!
//! Tier 1 (`Spec` in `main.rs`) puts one app in one `comp-host` behind a proxy, and
//! that is the whole of it — no control plane, no bus. This is the tier above: a
//! `comp-host` per NODE holding every app, a `comp-reconciler` converging them
//! against desired state, and a `comp-ingress` routing by `Host` header. All three
//! already exist and are measured; what did not exist is a way to INSTALL them.
//!
//! `just host-platform` is the localhost version of exactly this topology — NATS,
//! the control plane, the reconciler and one node, started in a `mktemp -d` under a
//! `trap kill`. These units are that recipe with the trap replaced by
//! `Restart=always` and the temp directory replaced by `StateDirectory`.
//!
//! ## Why this is a renderer and not a script
//!
//! The same reason the tier-1 renderer is: a pure function from a spec to text can
//! be read before it is trusted to a server, and tested without one. `holon fleet
//! render` prints what `holon fleet deploy` would install, and the tests below
//! assert the properties that actually matter — that the reconciler keeps its lease,
//! that the ingress is the only process facing the network, and that every flag
//! emitted exists on the binary that will receive it.
//!
//! ## What is deliberately NOT here
//!
//! No supervisor and no second scheduler. `comp-reconciler` already elects a leader
//! through a JetStream KV lease whose expiry IS the lease (`lattice/src/lease.rs`),
//! so a standby is a second unit on a second box and nothing else — which is the
//! gap ADR-0072 names, and it closes without code.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{bail, Result};
use serde::Deserialize;

/// One lattice, as a person writes it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fleet {
    /// The lattice name — the first subject token after `comp.`. Two lattices on one
    /// NATS never see each other's commands, so this is the isolation boundary
    /// between a staging fleet and a production one.
    pub lattice: String,
    /// Where the bus is. Every node, the reconciler and the ingress dial this.
    pub nats_url: String,
    /// The control plane the reconciler polls for desired state.
    pub platform_url: String,

    /// The boxes. Each runs a `comp-host` in node mode.
    pub nodes: Vec<Node>,

    /// Which box runs the ingress. It is the only process that faces the network,
    /// so it is named rather than defaulted.
    pub ingress: Ingress,

    /// Which boxes run a reconciler. More than one is the SUPPORTED case, not a
    /// mistake: they contend for a lease and exactly one wins, so a second entry
    /// here is a standby (ADR-0072).
    #[serde(default)]
    pub reconcilers: Vec<Reconciler>,

    /// Seconds between reconcile passes. Also the floor on how fast anything is
    /// noticed, other than an activation.
    #[serde(default)]
    pub interval: Option<u64>,
    /// Seconds an inventory entry survives without a heartbeat — the failover
    /// detection time. ADR-0035 measured a dead machine noticed 11-12s after the
    /// kill with this at 15.
    #[serde(default)]
    pub inventory_ttl: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    /// The node's name on the lattice. Must be stable across restarts: inventory is
    /// keyed by it, so a node that renames itself looks like one death and one birth.
    pub name: String,
    /// `host:port` an ingress can reach this node on. NOT loopback — unlike tier 1,
    /// a node must be reachable from whichever box holds the ingress.
    pub addr: String,
    /// Backing store for `wasi:keyvalue`. `sqlite` and `memory` are NODE-LOCAL, and
    /// the reconciler refuses to spread a stateful app across nodes that use them
    /// (ADR-0027) — so a real fleet wants `nats`.
    #[serde(default = "default_node_kv")]
    pub kv: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ingress {
    /// Which box it runs on. Free-form: it names a host to `scp` to, not a node.
    pub host: String,
    /// Where it listens. `127.0.0.1:8088` puts a TLS front (Caddy) in front of it,
    /// which is what `holon node ingress` renders and what ADR's reasoning assumes:
    /// TLS is not terminated by the ingress.
    #[serde(default = "default_ingress_addr")]
    pub addr: String,
    /// Requests in flight to ONE NODE before shedding with 503; 0 disables it.
    /// ADR-0041: with no bound an overloaded node answered p99 42 SECONDS with zero
    /// errors; with 64, p99 747ms and more useful work done.
    #[serde(default)]
    pub max_inflight: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reconciler {
    pub host: String,
    /// Seconds a leader survives without renewing. Failover takes up to this plus
    /// one interval, so it must be comfortably longer than `interval`.
    #[serde(default)]
    pub lease_ttl: Option<u64>,
}

fn default_node_kv() -> String {
    // nats, not sqlite: this tier exists because there is more than one box, and a
    // node-local store under a fleet is the bug ADR-0027 refuses at placement time.
    "nats".into()
}
fn default_ingress_addr() -> String {
    // Loopback: Caddy does ACME and terminates TLS, the ingress speaks plain HTTP
    // behind it. Reimplementing certificate handling inside a reverse proxy whose
    // whole job is to forward is work with a known-worse outcome.
    "127.0.0.1:8088".into()
}

/// Where the lattice binaries and their state live on a box.
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
        // A node that advertises loopback is unreachable from the ingress unless the
        // ingress happens to be on that same box — and it will fail at request time,
        // long after the deploy said it worked.
        if n.addr.starts_with("127.") || n.addr.starts_with("localhost:") {
            bail!(
                "node {} advertises {:?}, which no other box can reach — advertise the address an ingress can dial",
                n.name, n.addr
            );
        }
    }

    // A lease shorter than a pass expires before the holder can renew it, so
    // leadership would flap once per interval forever.
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

/// The hardening every lattice unit carries. Identical to the tier-1 set and for the
/// identical reason: these processes serve a network, and one of them holds the
/// platform secret.
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

/// A node: `comp-host` in lattice mode, holding every tenant placed on this box.
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
    // NO --state-dir, for the same reason tier 1 emits no --sqlite-path: comp-host
    // already defaults it to $STATE_DIRECTORY, which systemd sets for any unit
    // declaring StateDirectory= and which DynamicUser makes private to this unit.
    //
    // Passing it explicitly would be worse than redundant — systemd does not expand
    // ${STATE_DIRECTORY} inside ExecStart unless it was set with Environment=, so
    // the flag would arrive as that literal string and the node would write its
    // artifacts and instance ledger to a directory of that name.
    s.push('\n');
    s.push_str(&format!("StateDirectory=comp/node-{}\n", n.name));
    s.push_str(hardening());
    // wasmtime JITs. Same exemption as tier 1, spelled out where a reader will find
    // it rather than left as an absence.
    s.push_str("# wasmtime JITs, so it needs W^X — this one cannot be tightened.\nMemoryDenyWriteExecute=no\n");
    s.push_str("\n[Install]\nWantedBy=multi-user.target\n");
    s
}

/// The reconciler: the only process holding the platform secret.
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
    // The secret arrives as an environment file readable only by root before
    // systemd drops privileges — never as argv, which every process on the box can
    // read out of /proc.
    s.push_str(&format!("EnvironmentFile={}/reconciler.env\n", l.env_dir.display()));
    s.push_str(hardening());
    s.push_str("\n[Install]\nWantedBy=multi-user.target\n");
    s
}

/// The ingress: the door. Routes by `Host` header from inventory.
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

/// The reconciler's environment file. One secret, by reference.
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

    /// `MINIMAL` plus the two tunables — built by PREPENDING, because in TOML every
    /// key after a `[table]` header belongs to that table. Appending them to
    /// `MINIMAL` puts them inside `[ingress]`, which is the same trap the app spec
    /// warns about with "KEEP TABLES LAST".
    fn tuned() -> String {
        format!("interval = 3\ninventory_ttl = 15\n{MINIMAL}")
    }

    #[test]
    fn a_minimal_fleet_is_four_facts() {
        let f = fleet(MINIMAL);
        check(&f).unwrap();
        assert_eq!(f.nodes.len(), 1);
        // nats, not sqlite: a fleet is more than one box by definition, and a
        // node-local store under one is what ADR-0027 refuses.
        assert_eq!(f.nodes[0].kv, "nats");
        // Loopback, because Caddy is in front doing ACME.
        assert_eq!(f.ingress.addr, "127.0.0.1:8088");
    }

    #[test]
    fn a_node_advertising_loopback_is_refused() {
        // It would deploy clean and fail at the first request from any other box.
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
        // Renewal happens once per pass, so a lease that expires inside one interval
        // is lost every time it is held.
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
        // The lease makes exactly one of them act. This is ADR-0072's gap closing
        // with a unit rather than with code, so the spec must accept it.
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
        // The one exemption, and it is explained in the unit itself.
        assert!(u.contains("MemoryDenyWriteExecute=no"), "{u}");
        // The unit declares the directory and comp-host defaults to it. Passing the
        // flag would send the literal "${STATE_DIRECTORY}" — systemd does not expand
        // it in ExecStart — so its ABSENCE is the property worth asserting.
        assert!(!u.contains("--state-dir"), "comp-host defaults to $STATE_DIRECTORY: {u}");
        assert!(u.contains("StateDirectory=comp/node-node-1"), "{u}");
    }

    #[test]
    fn the_secret_reaches_the_reconciler_by_file_and_never_by_argv() {
        let f = fleet(MINIMAL);
        let r = Reconciler { host: "c1".into(), lease_ttl: None };
        let u = render_reconciler_unit(&f, &r, &FleetLayout::default());
        assert!(u.contains("EnvironmentFile=/etc/comp/reconciler.env"), "{u}");
        // Every process on the box can read another's argv out of /proc.
        assert!(!u.contains("--secret"), "the secret must not be on the command line: {u}");
        // And the rendered file carries no value to commit.
        assert!(render_reconciler_env().contains("PLATFORM_SECRET=\n"));
    }

    #[test]
    fn the_ingress_is_the_only_thing_that_faces_a_network() {
        let f = fleet(MINIMAL);
        let u = render_ingress_unit(&f, &FleetLayout::default());
        // Behind Caddy, on loopback. The node units advertise a routable address
        // because the ingress must dial them; the ingress itself does not.
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
        // inventory_ttl is the ingress's to honour — it is the failover detection
        // time, and the ingress is what stops offering traffic to a corpse.
        let iu = render_ingress_unit(&f, &FleetLayout::default());
        assert!(iu.contains("--inventory-ttl 15"), "{iu}");
    }

    /// The tier-1 renderer has a test asserting every flag it emits exists on
    /// `comp-host`, because a unit systemd would refuse to start is the failure this
    /// lane is most likely to produce. Same argument, three binaries.
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
                // Not silently skipped in CI: `cargo build --release` runs before
                // `cargo test` there, so absence means a real build gap locally.
                None => {
                    eprintln!("skipping {bin}: not built — `cargo build --release` in its workspace");
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
        // Each binary lives in its own workspace's target dir; the renderer names
        // /usr/local/bin, which is where they land on a box, not here.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent()?;
        let ws = match bin {
            "comp-host" => "host",
            _ => "reconciler",
        };
        let path = root.join(ws).join("target/release").join(bin);
        if !path.exists() {
            return None;
        }
        let out = std::process::Command::new(&path).arg("--help").output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}
