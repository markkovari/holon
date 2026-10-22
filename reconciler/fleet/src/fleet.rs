//! Start a fleet, drive it, read it; every process is a child killed on drop.

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `Fixed` is never deleted, so the jetstream store and SQLite DB survive separate launches.
const APPLIER_SECRET: &str = "test-secret";

/// Made once per kept lattice and never rotated: a new key cannot read what the old one sealed.
fn master_key(dir: &std::path::Path) -> String {
    use base64::Engine;
    let path = dir.join("master-key");
    if let Ok(k) = std::fs::read_to_string(&path) {
        return k.trim().to_string();
    }
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("random bytes for the master key");
    let k = base64::engine::general_purpose::STANDARD.encode(bytes);
    std::fs::write(&path, &k).expect("writing the master key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    k
}

enum Scratch {
    Owned(tempfile::TempDir),
    Fixed(std::path::PathBuf),
}

impl Scratch {
    fn path(&self) -> &std::path::Path {
        match self {
            Scratch::Owned(t) => t.path(),
            Scratch::Fixed(p) => p.as_path(),
        }
    }
}

/// Keeps the node logs when asked; the scratch directory still exists during `drop`.
impl Drop for Fleet {
    fn drop(&mut self) {
        let Some(dest) = std::env::var_os("COMP_FLEET_KEEP_LOGS") else { return };
        let dest = std::path::PathBuf::from(dest);
        if std::fs::create_dir_all(&dest).is_err() {
            return;
        }
        let Ok(entries) = std::fs::read_dir(self.dir.path()) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "log") {
                if let Some(name) = path.file_name() {
                    let _ = std::fs::copy(&path, dest.join(name));
                }
            }
        }
        eprintln!("fleet logs kept in {}", dest.display());
    }
}

pub struct Fleet {
    dir: Scratch,
    /// So a second reconciler can be pointed at the same stub control plane.
    platform_port: u16,
    /// Every reconciler started, in order, so a test can kill the leader.
    reconcilers: Vec<Kill>,
    /// Killed by pid: every reconciler on this lattice shares a command line.
    first_reconciler_pid: u32,
    /// One per node, in order, so a benchmark can read a host's memory.
    host_pids: Vec<u32>,
    /// The port each host serves HTTP on, so load can bypass the ingress.
    host_ports: Vec<u16>,
    _children: Vec<Kill>,
    pub nats_url: String,
    pub lattice: String,
    pub ingress_port: u16,
}

/// A running load generator. Threads rather than async: a blocking client counts answers directly.
pub struct Load {
    stop: Arc<AtomicBool>,
    ok: Arc<AtomicU64>,
    shed: Arc<AtomicU64>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Load {
    /// Returns (answered, refused).
    pub fn stop(self) -> (u64, u64) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads {
            let _ = t.join();
        }
        (self.ok.load(Ordering::Relaxed), self.shed.load(Ordering::Relaxed))
    }
}

/// Open-loop: a fixed connection count pins mean latency to `concurrency / rps` (Little's law).
pub struct Report {
    /// Requests the schedule called for.
    pub due: u64,
    pub ok: u64,
    pub shed: u64,
    pub failed: u64,
    /// A bare shed count cannot tell the ingress declining load from a missing route.
    pub statuses: std::collections::BTreeMap<u16, u64>,
    /// What the first refusal SAID: "nothing is placed" and "everything is saturated" are both 503.
    pub first_refusal: Option<String>,
    /// Clustered at the start means the harness raced something; spread means real refusal.
    pub refusal_window: Option<(f64, f64)>,
    /// Tells the system refusing from the load generator running out of sockets.
    pub first_error: Option<String>,
    /// Microseconds from when each request was due, not sent: avoids coordinated omission.
    pub latencies: Vec<u64>,
}

impl Report {
    pub fn pct(&self, p: f64) -> f64 {
        if self.latencies.is_empty() {
            return 0.0;
        }
        let i = ((self.latencies.len() - 1) as f64 * p / 100.0).round() as usize;
        self.latencies[i] as f64 / 1000.0
    }

    /// Below the requested rate, the percentiles describe a system in backlog.
    pub fn rps(&self, secs: f64) -> f64 {
        self.ok as f64 / secs
    }
}

pub struct OpenLoad {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<Report>>,
}

impl OpenLoad {
    pub fn stop(self) -> Report {
        self.stop.store(true, Ordering::Relaxed);
        let mut all = Report {
            due: 0,
            ok: 0,
            shed: 0,
            failed: 0,
            statuses: Default::default(),
            first_refusal: None,
            refusal_window: None,
            first_error: None,
            latencies: Vec::new(),
        };
        for t in self.threads {
            if let Ok(r) = t.join() {
                all.due += r.due;
                all.ok += r.ok;
                all.shed += r.shed;
                all.failed += r.failed;
                all.first_error = all.first_error.or(r.first_error);
                all.first_refusal = all.first_refusal.or(r.first_refusal);
                all.refusal_window = match (all.refusal_window, r.refusal_window) {
                    (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.max(b.1))),
                    (a, b) => a.or(b),
                };
                for (code, n) in r.statuses {
                    *all.statuses.entry(code).or_default() += n;
                }
                all.latencies.extend(r.latencies);
            }
        }
        all.latencies.sort_unstable();
        all
    }
}

/// Both ends use this: `Fleet` configures NATS with it and `comp-goalrun` refuses larger trees.
/// 8 MB is NATS's recommended ceiling; 64 MB the hard maximum.
pub const MAX_TREE_BYTES: usize = 8 * 1024 * 1024;

/// Leaves room for the envelope; 90% is a margin, not a measurement.
pub fn max_tree_payload() -> usize {
    MAX_TREE_BYTES / 10 * 9
}

/// Walks from the current executable: `CARGO_BIN_EXE_*` exists only inside an integration test.
pub fn bin_path(name: &str) -> std::path::PathBuf {
    if let Ok(p) = std::env::var(format!("COMP_{}_BIN", name.replace('-', "_").to_uppercase())) {
        return std::path::PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        for dir in exe.parent().into_iter().chain(exe.parent().and_then(|p| p.parent())) {
            let c = dir.join(name);
            if c.exists() {
                return c;
            }
        }
    }
    repo_root().join(format!("target/release/{name}"))
}

/// Only the owner's tests get `CARGO_BIN_EXE_<name>`; the job builds it first (docs/adr/0115).
pub fn built_bin(name: &str) -> std::path::PathBuf {
    let p = bin_path(name);
    assert!(
        p.exists(),
        "{name} is not built: {} does not exist. It belongs to another package than this suite, so \
         `cargo test` does not build it for you: `cargo build --bin {name}` (with \
         `--release` when this suite runs with it)",
        p.display()
    );
    p
}

/// `CARGO_MANIFEST_DIR` is baked in at compile time; the override lets `bench/` drive a second box.
pub fn repo_root() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("COMP_REPO_ROOT") {
        return std::path::PathBuf::from(p);
    }
    root_from_manifest()
}

/// The compiled-from repo, ignoring `COMP_REPO_ROOT`: gates judge their own sandbox checkout.
pub fn root_from_manifest() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("comp-fleet sits two directories below the repository root")
        .to_path_buf()
}

/// Keeps one demo app's fleet serving forever; shared by the `comp-<app>serve` bins.
pub fn serve_fixture(tag: &str, wasm_name: &str, fixture_stem: &str) -> ! {
    let home = std::env::var("HOME").unwrap();
    let key = format!("{home}/.comp-secrets/anthropic");
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let art = vec![format!("gate={}", dir.join(wasm_name).display())];
    let fixture = repo_root().join(format!("fixtures/{fixture_stem}.yaml"));
    let fleet = Fleet::start_with_secrets(
        tag,
        &[fixture.to_str().unwrap()],
        &art,
        &[format!("vault://acme/anthropic=@{key}")],
    );
    println!("{}_INGRESS_PORT={}", tag.to_uppercase(), fleet.ingress_port);
    println!("host header: {tag}.acme.test");
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// To a file, not /dev/null: several assertions are about what a process said.
fn spawn_logged(name: &str, cmd: &mut Command, log: &std::path::Path) -> Kill {
    let f =
        std::fs::File::create(log).unwrap_or_else(|e| panic!("creating {}: {e}", log.display()));
    let err = f.try_clone().unwrap();
    Kill(cmd.stdout(Stdio::from(f)).stderr(Stdio::from(err)).spawn().unwrap_or_else(|e| {
        // A missing `comp-*` is usually another package's, which cargo does not build here.
        let hint = if e.kind() == std::io::ErrorKind::NotFound && name.starts_with("comp-") {
            format!(
                " — build it with `cargo build --bin {name}`; `cargo test` \
                         builds it for you only in the package that owns it"
            )
        } else {
            String::new()
        };
        panic!("spawning {name}: {e}{hint}")
    }))
}

/// Hash-derived ports collided across parallel tests; a small bind race remains.
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").expect("no free port").local_addr().unwrap().port()
}

/// Replaces a fixed sleep, which on a loaded runner looks like a process that never started.
fn wait_for_port(addr: &str, within: Duration) {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(addr).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("nothing listening on {addr} after {within:?}");
}

impl Fleet {
    /// `specs` are YAML paths relative to `comp/`; `max_inflight` sets the ingress shedding bound.
    pub fn start(lattice: &str, specs: &[&str], nodes: u16, max_inflight: Option<u32>) -> Self {
        Self::start_with_kv(lattice, specs, nodes, max_inflight, None)
    }

    /// `Some("sqlite")` gives every node its own file, where a spread stateful app is refused.
    pub fn start_with_kv(
        lattice: &str,
        specs: &[&str],
        nodes: u16,
        max_inflight: Option<u32>,
        kv: Option<&str>,
    ) -> Self {
        // What production runs: pooling on (ADR-0054), read cache off (ADR-0063).
        Self::start_full(
            lattice,
            specs,
            &[],
            &[],
            nodes,
            max_inflight,
            kv,
            true,
            0,
            &[],
            false,
            &[],
            None,
        )
    }

    /// A reference a spec grants and this list omits must never start (ADR-0051).
    pub fn start_with_secrets(
        lattice: &str,
        specs: &[&str],
        artifacts: &[String],
        secrets: &[String],
    ) -> Self {
        Self::start_full(
            lattice,
            specs,
            artifacts,
            secrets,
            1,
            None,
            None,
            true,
            0,
            &[],
            false,
            &[],
            None,
        )
    }

    /// Driven by the real control plane, for tests of the platform's own API (ADR-0078).
    pub fn start_with_platform(lattice: &str, nodes: u16) -> Self {
        Self::start_full(lattice, &[], &[], &[], nodes, None, None, true, 0, &[], true, &[], None)
    }

    /// State lives in `dir`, created if missing and never deleted, so it survives relaunches.
    pub fn start_with_platform_in_dir(
        lattice: &str,
        nodes: u16,
        dir: std::path::PathBuf,
        host_args: &[String],
    ) -> Self {
        Self::start_full(
            lattice,
            &[],
            &[],
            &[],
            nodes,
            None,
            None,
            true,
            0,
            &[],
            true,
            host_args,
            Some(dir),
        )
    }

    /// For a host flag no other entry point sets, such as `--blob s3`.
    pub fn start_with_host_args(
        lattice: &str,
        specs: &[&str],
        artifacts: &[String],
        host_args: &[String],
    ) -> Self {
        Self::start_full(
            lattice,
            specs,
            artifacts,
            &[],
            1,
            None,
            None,
            true,
            0,
            &[],
            false,
            host_args,
            None,
        )
    }

    pub fn platform_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.platform_port)
    }

    /// `labels[i]` applies to node `i+1`; without labels a constrained app is unschedulable.
    pub fn start_with_labels(
        lattice: &str,
        specs: &[&str],
        artifacts: &[String],
        labels: &[&str],
    ) -> Self {
        Self::start_labelled_kv(lattice, specs, artifacts, labels, None)
    }

    /// `memory` suits hop measurement: JetStream round trips hide a cross-node call (ADR-0057).
    pub fn start_labelled_kv(
        lattice: &str,
        specs: &[&str],
        artifacts: &[String],
        labels: &[&str],
        kv: Option<&str>,
    ) -> Self {
        Self::start_full(
            lattice,
            specs,
            artifacts,
            &[],
            labels.len().max(1) as u16,
            None,
            kv,
            true,
            0,
            labels,
            false,
            &[],
            None,
        )
    }

    /// Two or more nodes: on one, the cache invalidates its own writes and is never caught stale.
    pub fn start_with_cache(lattice: &str, specs: &[&str], nodes: u16, cache_ms: u64) -> Self {
        Self::start_full(
            lattice,
            specs,
            &[],
            &[],
            nodes,
            None,
            None,
            true,
            cache_ms,
            &[],
            false,
            &[],
            None,
        )
    }

    /// The control plane's internal-API secret; it binds to loopback only.
    pub fn applier_secret(&self) -> &'static str {
        APPLIER_SECRET
    }

    #[allow(clippy::too_many_arguments)]
    fn start_full(
        lattice: &str,
        specs: &[&str],
        artifacts: &[String],
        secrets: &[String],
        nodes: u16,
        max_inflight: Option<u32>,
        kv: Option<&str>,
        pool: bool,
        cache_ms: u64,
        labels: &[&str],
        real_platform: bool,
        host_args: &[String],
        scratch_dir: Option<std::path::PathBuf>,
    ) -> Self {
        let root = repo_root();
        let host_bin = std::env::var("COMP_HOST_BIN")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| root.join("target/release/comp-host"));
        assert!(
            host_bin.exists(),
            "missing {} — cargo build --release in host/",
            host_bin.display()
        );

        let (nats_port, platform_port, ingress_port) = (free_port(), free_port(), free_port());
        let dir = match scratch_dir {
            Some(p) => {
                std::fs::create_dir_all(&p).expect("creating the fixed scratch dir");
                Scratch::Fixed(p)
            }
            None => Scratch::Owned(tempfile::tempdir().unwrap()),
        };
        let sp = dir.path().to_path_buf();
        // Set COMP_FLEET_KEEP_LOGS to keep node logs past drop.
        if let Some(dest) = std::env::var_os("COMP_FLEET_KEEP_LOGS") {
            eprintln!("fleet logs: {} (copied to {:?} on shutdown)", sp.display(), dest);
        }
        let mut children = Vec::new();

        // A base tree is one message and NATS defaults `max_payload` to 1 MB; only `-c` can set it.
        let conf = sp.join("nats.conf");
        std::fs::write(&conf, format!("max_payload: {MAX_TREE_BYTES}\n"))
            .expect("writing the nats config");
        let mut nats = Command::new("nats-server");
        nats.arg("-c").arg(&conf).args(["-js", "-sd"]).arg(sp.join("nats")).args([
            "-a",
            "127.0.0.1",
            "-p",
            &nats_port.to_string(),
        ]);
        children.push(spawn_logged("nats-server", &mut nats, &sp.join("nats.log")));
        wait_for_port(&format!("127.0.0.1:{nats_port}"), Duration::from_secs(20));

        // `comp-stub` serves fixtures only; spawning an environment needs `platform-domain`.
        if real_platform {
            // Derived when absent, which otherwise surfaced two workspaces into `just test`.
            let legacy = root.join("components/target/platform_domain.composed.wasm");
            let component = if legacy.is_file() {
                legacy
            } else {
                let catalog = crate::plug::Catalog::scan(&crate::plug::default_dirs(&root));
                crate::plug::compose_to(
                    "platform-domain",
                    &catalog,
                    &root.join("components/target/composed"),
                )
                .unwrap_or_else(|e| {
                    panic!("composing platform-domain: {e} — `cargo xtask build --force` first")
                })
            };
            let mut cp = Command::new(&host_bin);
            cp.current_dir(&root)
                .arg("--component")
                .arg(&component)
                .args(["--addr", &format!("127.0.0.1:{platform_port}")])
                .args(["--tenant", "platform", "--app", "control-plane"])
                .args(["--config", &format!("applier-secret={APPLIER_SECRET}")])
                .args(["--config", "ingress-suffix=test"]);
            // A kept lattice (the console's) holds real secrets: its own JetStream and its own key.
            if let Scratch::Fixed(_) = dir {
                cp.args(["--kv", "nats", "--nats-url", &format!("nats://127.0.0.1:{nats_port}")])
                    .args(["--config", &format!("master-key={}", master_key(&sp))]);
            } else {
                // Without a master key every vault write is refused; this key guards nothing real.
                cp.args(["--kv", "sqlite", "--sqlite-path"])
                    .arg(sp.join("platform.db"))
                    .args(["--config", "master-key=Y29tcC10ZXN0LW9ubHktbWFzdGVyLWtleS0zMmJ5dGU="]);
            }
            // Admission control (ADR-0082): set low to see the refusal.
            if let Ok(lag) = std::env::var("COMP_MAX_PLACEMENT_LAG") {
                cp.args(["--config", &format!("max-placement-lag={lag}")]);
            }
            // Report age before admission fails closed.
            if let Ok(per) = std::env::var("COMP_MAX_PLACEMENT_LAG_PER_NODE") {
                cp.args(["--config", &format!("max-placement-lag-per-node={per}")]);
            }
            // Operator-granted egress for every tenant, never a tenant's own (ADR-0008).
            if let Ok(egress) = std::env::var("COMP_DEFAULT_EGRESS") {
                cp.args(["--config", &format!("default-egress={egress}")]);
            }
            if let Ok(age) = std::env::var("COMP_STATUS_MAX_AGE") {
                cp.args(["--config", &format!("status-max-age={age}")]);
            }
            children.push(spawn_logged("control-plane", &mut cp, &sp.join("platform.log")));
            std::thread::sleep(Duration::from_secs(2));
        }
        let mut stub = Command::new(bin_path("comp-stub"));
        stub.current_dir(&root).args(["--port", &platform_port.to_string()]);
        for s in specs {
            stub.args(["--spec", s]);
        }
        if artifacts.is_empty() {
            // Derived from gate-domain's imports, cached by content; a hand-made one still wins.
            let legacy = root.join("components/target/gate_domain.composed.wasm");
            let gate = if legacy.is_file() {
                legacy
            } else {
                let catalog = crate::plug::Catalog::scan(&crate::plug::default_dirs(&root));
                crate::plug::compose_to(
                    "gate-domain",
                    &catalog,
                    &root.join("components/target/composed"),
                )
                .unwrap_or_else(|e| {
                    panic!("composing the default `gate` artifact: {e} — `cargo xtask build --force` first")
                })
            };
            stub.args(["--artifact", &format!("gate={}", gate.display())]);
        } else {
            for a in artifacts {
                stub.args(["--artifact", a]);
            }
        }
        for s in secrets {
            stub.args(["--secret", s]);
        }
        if !real_platform {
            children.push(spawn_logged("comp-stub", &mut stub, &sp.join("stub.log")));
        }

        let nats_url = format!("nats://127.0.0.1:{nats_port}");
        let mut host_pids = Vec::new();
        let mut host_ports = Vec::new();
        for n in 1..=nodes {
            let host_port = free_port();
            host_ports.push(host_port);
            let mut c = Command::new(&host_bin);
            c.current_dir(&root)
                .args([
                    "--lattice-nats",
                    &nats_url,
                    "--node",
                    &format!("n{n}"),
                    "--lattice",
                    lattice,
                ])
                .args(["--addr", &format!("127.0.0.1:{host_port}")])
                .args(["--advertise-addr", &format!("127.0.0.1:{host_port}")])
                // Where granted secrets are fetched from (ADR-0051).
                .args(["--platform-url", &format!("http://127.0.0.1:{platform_port}")])
                .arg("--state-dir")
                .arg(sp.join(format!("n{n}")));
            if let Some(kv) = kv {
                c.args(["--kv", kv]).arg("--sqlite-path").arg(sp.join(format!("n{n}/kv.db")));
            }
            if !pool {
                c.arg("--no-pool");
            }
            if cache_ms > 0 {
                c.args(["--kv-cache-ms", &cache_ms.to_string()]);
            }
            if let Some(l) = labels.get((n - 1) as usize) {
                c.args(["--label", l]);
            }
            c.args(host_args);
            // Loopback is private (ADR-0008); opt-in, and it widens only the address check.
            if std::env::var_os("COMP_FLEET_ALLOW_PRIVATE_EGRESS").is_some() {
                c.arg("--allow-private-egress");
            }
            let child = spawn_logged("comp-host", &mut c, &sp.join(format!("n{n}.log")));
            host_pids.push(child.0.id());
            children.push(child);
        }
        std::thread::sleep(Duration::from_secs(2));

        let mut rec = Command::new(bin_path("comp-reconciler"));
        rec.current_dir(&root)
            .args(["--platform-url", &format!("http://127.0.0.1:{platform_port}")])
            .args(["--secret", APPLIER_SECRET, "--nats-url", &nats_url, "--lattice", lattice])
            // Short lease, so a failover test does not wait production's 30s.
            .args(["--interval", "3", "--lease-ttl", "6"]);
        let rec_child = spawn_logged("comp-reconciler", &mut rec, &sp.join("rec.log"));
        let first_reconciler_pid = rec_child.0.id();
        children.push(rec_child);

        let mut ing = Command::new(bin_path("comp-ingress"));
        ing.current_dir(&root).args(["--addr", &format!("127.0.0.1:{ingress_port}")]).args([
            "--nats-url",
            &nats_url,
            "--lattice",
            lattice,
            "--refresh-secs",
            "2",
        ]);
        if let Some(m) = max_inflight {
            ing.args(["--max-inflight", &m.to_string()]);
        }
        // A real run's guest call can outlive the 30s default backend timeout.
        if let Ok(t) = std::env::var("COMP_FLEET_BACKEND_TIMEOUT") {
            ing.args(["--backend-timeout", &t]);
        }
        children.push(spawn_logged("comp-ingress", &mut ing, &sp.join("ingress.log")));

        Self {
            dir,
            platform_port,
            reconcilers: Vec::new(),
            first_reconciler_pid,
            host_pids,
            host_ports,
            _children: children,
            nats_url,
            lattice: lattice.to_string(),
            ingress_port,
        }
    }

    /// A SECOND ingress against the same lattice; it holds no state beyond a cache of inventory.
    pub fn second_ingress(&mut self) -> u16 {
        let port = free_port();
        let mut ing = Command::new(bin_path("comp-ingress"));
        ing.current_dir(repo_root()).args(["--addr", &format!("127.0.0.1:{port}")]).args([
            "--nats-url",
            &self.nats_url,
            "--lattice",
            &self.lattice,
            "--refresh-secs",
            "2",
        ]);
        self._children.push(spawn_logged(
            "comp-ingress-b",
            &mut ing,
            &self.dir.path().join("ingress-b.log"),
        ));
        port
    }

    /// No separate readiness signal: retry the operation the test cares about. Four probes chosen
    /// apart from the measured thing passed alone and failed under load; `why` prints on timeout.
    pub fn until<T>(
        &self,
        what: &str,
        within: Duration,
        mut f: impl FnMut() -> std::result::Result<T, String>,
    ) -> T {
        let deadline = Instant::now() + within;
        // No initializer: the loop either returns or assigns before anything reads this.
        let mut last;
        loop {
            match f() {
                Ok(v) => return v,
                Err(why) => last = why,
            }
            if Instant::now() >= deadline {
                panic!(
                    "{what} never worked within {within:?} — last answer: {last}\n\
                     --- node n1 ---\n{}\n--- reconciler ---\n{}\n--- control plane ---\n{}",
                    self.node_log("n1"),
                    self.reconciler_log(),
                    self.platform_log()
                );
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// Inventory is the only honest answer: an empty-table ingress still answers by activating.
    pub fn wait_for_placement(&self, host: &str, within: Duration) -> bool {
        let (url, lattice) = (self.nats_url.clone(), self.lattice.clone());
        let host = host.to_ascii_lowercase();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let Ok(inv) =
                comp_lattice::nats::NatsLattice::connect(&url, &lattice, Duration::from_secs(15))
                    .await
            else {
                return false;
            };
            let deadline = Instant::now() + within;
            while Instant::now() < deadline {
                if let Ok(entries) = comp_lattice::Inventory::read_all(&inv).await {
                    let placed = entries.iter().any(|e| {
                        serde_json::from_slice::<serde_json::Value>(
                            &comp_lattice::snapshot::expand(e.value.clone()),
                        )
                        .ok()
                        .and_then(|v| v["instances"].as_array().cloned())
                        .is_some_and(|is| {
                            is.iter().any(|i| {
                                i["ingress_host"]
                                    .as_str()
                                    .is_some_and(|h| h.eq_ignore_ascii_case(&host))
                            })
                        })
                    });
                    if placed {
                        return true;
                    }
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            false
        })
    }

    /// What an ingress said. `""` is the first one, any other name is a suffix (`"-b"` for the second).
    pub fn ingress_log(&self, which: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(format!("ingress{which}.log")))
            .unwrap_or_else(|e| format!("(no ingress{which}.log: {e})"))
    }

    /// It should stand by: exactly one holds the lease (ADR-0072). `n` names its log.
    pub fn second_reconciler(&mut self, n: &str) -> &Kill {
        let mut rec = Command::new(bin_path("comp-reconciler"));
        rec.current_dir(repo_root())
            .args(["--platform-url", &format!("http://127.0.0.1:{}", self.platform_port)])
            .args(["--secret", APPLIER_SECRET, "--nats-url", &self.nats_url])
            .args(["--lattice", &self.lattice, "--interval", "3", "--lease-ttl", "6"]);
        let k = spawn_logged(
            &format!("comp-reconciler-{n}"),
            &mut rec,
            &self.dir.path().join(format!("rec-{n}.log")),
        );
        self.reconcilers.push(k);
        self.reconcilers.last().unwrap()
    }

    pub fn reconciler_log_named(&self, n: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(format!("rec-{n}.log"))).unwrap_or_default()
    }

    /// By pid: a standby is started with the same command line by design.
    pub fn kill_first_reconciler(&mut self) {
        let _ = std::process::Command::new("kill")
            .args(["-9", &self.first_reconciler_pid.to_string()])
            .status();
    }

    /// SIGKILL: the lattice must notice by itself, as on power loss or an OOM kill.
    pub fn kill_host(&self, n: u16) -> Option<u32> {
        let pid = *self.host_pids.get((n as usize).saturating_sub(1))?;
        let _ = std::process::Command::new("kill").args(["-9", &pid.to_string()]).status();
        Some(pid)
    }

    pub fn node_count(&self) -> usize {
        self.host_pids.len()
    }

    /// Stops the last-started process, e.g. one ingress so the other carries on.
    pub fn kill_last(&mut self) {
        self._children.pop();
    }

    /// `x-comp-node` is the only way to see the balance from outside.
    pub fn who_answers(
        &self,
        port: u16,
        n: usize,
    ) -> (std::collections::BTreeMap<String, usize>, usize) {
        let client =
            reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build().unwrap();
        let mut seen = std::collections::BTreeMap::new();
        let mut failed = 0;
        for i in 0..n {
            let r = client
                .post(format!("http://127.0.0.1:{port}/api/ratelimit"))
                .header("host", "shop.eve.test")
                .json(&serde_json::json!({
                    "key": format!("ha-{i}"), "capacity": 100_000_000u64, "refill": 100_000_000u64
                }))
                .send();
            match r {
                Ok(r) if r.status().is_success() => {
                    let node = r
                        .headers()
                        .get("x-comp-node")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("?")
                        .to_string();
                    *seen.entry(node).or_insert(0) += 1;
                }
                _ => failed += 1,
            }
        }
        (seen, failed)
    }

    /// Shared so a benchmark measures the same fleet the tests assert on.
    pub fn start_bench(
        lattice: &str,
        spec_dir: &str,
        artifacts: &[String],
        nodes: u16,
        pool: bool,
        // With `nats` the number measures the bus.
        kv: Option<&str>,
    ) -> Self {
        Self::start_full(
            lattice,
            &[spec_dir],
            artifacts,
            &[],
            nodes,
            None,
            kv,
            pool,
            0,
            &[],
            false,
            &[],
            None,
        )
    }

    /// The host process for node `n`, so a caller can read its RSS.
    pub fn host_pid(&self, n: u16) -> Option<u32> {
        self.host_pids.get((n as usize).saturating_sub(1)).copied()
    }

    /// How many instances this node reports having started.
    pub fn started_count(&self) -> usize {
        self.node_log("n1").matches("comp-host: started ").count()
    }

    /// (shared, from disk, compiled), read from the host's own log.
    pub fn module_arrivals(&self, n: u16) -> (usize, usize, usize) {
        let log = self.node_log(&format!("n{n}"));
        (
            log.matches(" shared ").count(),
            log.matches(" cache-load ").count(),
            log.matches(" compile ").count(),
        )
    }

    pub fn state_dir(&self) -> std::path::PathBuf {
        self.dir.path().to_path_buf()
    }

    /// A node's own log, for the timings and warnings it prints about itself.
    pub fn node_log(&self, node: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(format!("{node}.log"))).unwrap_or_default()
    }

    pub fn reconciler_log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("rec.log")).unwrap_or_default()
    }

    /// A refused manifest and a dead control plane look identical from the node and reconciler.
    pub fn platform_log(&self) -> String {
        let stub = std::fs::read_to_string(self.dir.path().join("stub.log")).unwrap_or_default();
        let real =
            std::fs::read_to_string(self.dir.path().join("platform.log")).unwrap_or_default();
        format!("{stub}{real}")
    }

    /// Replicas the fleet is running, straight from inventory.
    pub fn replicas(&self) -> u32 {
        let out = Command::new(bin_path("comp-bench"))
            .args(["inventory", "--nats-url", &self.nats_url, "--lattice", &self.lattice])
            .output();
        let Ok(out) = out else { return 0 };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.trim().strip_prefix("total ")?.split_whitespace().next()?.parse().ok())
            .unwrap_or(0)
    }

    /// Answers and refusals counted apart: conflating them reads a shed storm as an outage.
    pub fn load(&self, host: &str, threads: usize, _max: Duration) -> Load {
        let stop = Arc::new(AtomicBool::new(false));
        let (ok, shed) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let url = format!("http://127.0.0.1:{}/api/ratelimit", self.ingress_port);
        let mut handles = Vec::new();
        for _ in 0..threads {
            let (stop, ok, shed, url, host) =
                (stop.clone(), ok.clone(), shed.clone(), url.clone(), host.to_string());
            handles.push(std::thread::spawn(move || {
                let client = reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .unwrap();
                while !stop.load(Ordering::Relaxed) {
                    let res = client
                        .post(&url)
                        .header("host", &host)
                        .json(&serde_json::json!({
                            "key": "load", "capacity": 100_000_000u64, "refill": 100_000_000u64
                        }))
                        .send();
                    match res {
                        Ok(r) if r.status().is_success() => ok.fetch_add(1, Ordering::Relaxed),
                        Ok(_) => shed.fetch_add(1, Ordering::Relaxed),
                        Err(_) => shed.fetch_add(1, Ordering::Relaxed),
                    };
                }
            }));
        }
        Load { stop, ok, shed, threads: handles }
    }

    /// Fixed arrival rate, timed from due, each worker walking every host. `rate: None` is a closed
    /// loop, whose percentiles describe concurrency the harness pinned.
    pub fn open_load(
        &self,
        hosts: &[String],
        port: u16,
        rate: Option<u32>,
        workers: usize,
    ) -> OpenLoad {
        let stop = Arc::new(AtomicBool::new(false));
        let url = format!("http://127.0.0.1:{port}/api/ratelimit");
        let gap = rate.map(|r| Duration::from_secs_f64(1.0 / (r as f64 / workers as f64).max(1.0)));
        let mut threads = Vec::new();
        for w in 0..workers {
            let (stop, url) = (stop.clone(), url.clone());
            // Offset per worker so apps are not all hot and cold together.
            let hosts = hosts.to_vec();
            let mut turn = w;
            threads.push(std::thread::spawn(move || {
                let client = reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()
                    .unwrap();
                let mut r = Report {
                    due: 0,
                    ok: 0,
                    shed: 0,
                    failed: 0,
                    statuses: Default::default(),
                    first_refusal: None,
                    refusal_window: None,
                    first_error: None,
                    latencies: Vec::new(),
                };
                let start = Instant::now();
                let mut n: u32 = 0;
                while !stop.load(Ordering::Relaxed) {
                    let due = match gap {
                        Some(gap) => {
                            let due = start + gap.mul_f64(n as f64);
                            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                                std::thread::sleep(wait);
                            }
                            due
                        }
                        None => Instant::now(),
                    };
                    n += 1;
                    r.due += 1;
                    let host = &hosts[turn % hosts.len()];
                    turn += 1;
                    let res = client
                        .post(&url)
                        .header("host", host)
                        .json(&serde_json::json!({
                            "key": "m", "capacity": 100_000_000u64, "refill": 100_000_000u64
                        }))
                        .send();
                    match res {
                        Ok(resp) if resp.status().is_success() => {
                            r.ok += 1;
                            r.latencies.push(due.elapsed().as_micros() as u64);
                        }
                        Ok(resp) => {
                            r.shed += 1;
                            *r.statuses.entry(resp.status().as_u16()).or_default() += 1;
                            let at = start.elapsed().as_secs_f64();
                            r.refusal_window = Some(match r.refusal_window {
                                Some((f, _)) => (f, at),
                                None => (at, at),
                            });
                            if r.first_refusal.is_none() {
                                r.first_refusal =
                                    Some(resp.text().unwrap_or_default().trim().to_string());
                            }
                        }
                        Err(e) => {
                            r.failed += 1;
                            r.first_error.get_or_insert_with(|| e.to_string());
                        }
                    }
                }
                r
            }));
        }
        OpenLoad { stop, threads }
    }

    pub fn host_port(&self, n: u16) -> Option<u16> {
        self.host_ports.get((n as usize).saturating_sub(1)).copied()
    }

    /// Poll until a request to `host` is answered, or give up.
    pub fn serves(&self, host: &str, within: Duration) -> bool {
        let client =
            reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build().unwrap();
        let url = format!("http://127.0.0.1:{}/api/ratelimit", self.ingress_port);
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            let ok = client
                .post(&url)
                .header("host", host)
                .json(&serde_json::json!({
                    "key": "probe", "capacity": 100_000_000u64, "refill": 100_000_000u64
                }))
                .send()
                .map(|r| r.status().is_success())
                .unwrap_or(false);
            if ok {
                return true;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        false
    }
}

#[cfg(test)]
mod tests {
    /// A wrong depth fails nowhere near the cause.
    #[test]
    fn the_repo_root_is_the_repository_and_not_the_directory_this_package_sits_in() {
        let root = super::root_from_manifest();
        for marker in ["reconciler/Cargo.toml", "lattice/Cargo.toml", "components/Cargo.toml"] {
            assert!(root.join(marker).is_file(), "{} is not the repository root", root.display());
        }
    }
}
