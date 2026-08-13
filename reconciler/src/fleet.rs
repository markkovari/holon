//! Start a fleet, drive it, read it. Shared by the integration tests.
//!
//! Every process is a child killed on drop, so a failed assertion cannot leave a
//! lattice running — the failure mode that made the old bash benchmarks leak
//! `nats-server`s across a session.

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

pub struct Fleet {
    dir: tempfile::TempDir,
    /// The stub control plane's port, so a second reconciler can be pointed at
    /// the same one the first is using.
    platform_port: u16,
    /// Every reconciler started, in order, so a test can kill the leader.
    reconcilers: Vec<Kill>,
    /// The first reconciler's pid. Killed by pid rather than by pattern, because
    /// every reconciler on this lattice shares a command line.
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

/// A running load generator. Threads rather than async: what is being measured is
/// how many requests got answered, and a blocking client counts that directly.
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

/// What an open-loop run measured.
///
/// Separate from `Load` because it answers a different question. `Load` counts
/// completions under a fixed number of connections, which pins concurrency and
/// makes mean latency an algebraic restatement of throughput — Little's law,
/// `concurrency / rps`, exactly, to two decimals, for every cell ADR-0053 and
/// 0054 published. That number cannot be wrong and cannot be informative.
pub struct Report {
    /// Requests the schedule called for.
    pub due: u64,
    pub ok: u64,
    pub shed: u64,
    pub failed: u64,
    /// How many of each non-2xx status came back. A bare "shed" count cannot
    /// distinguish the ingress declining load from a route that is not there
    /// yet, and those want opposite fixes.
    pub statuses: std::collections::BTreeMap<u16, u64>,
    /// What the first refusal SAID. There is more than one 503 in the ingress —
    /// "nothing is placed" and "everything is saturated" are opposite problems —
    /// and a status code alone cannot tell them apart.
    pub first_refusal: Option<String>,
    /// Seconds into the run when the first and last refusal happened. Start-of-run
    /// clustering means the harness raced something; spread across the window
    /// means the platform is genuinely refusing.
    pub refusal_window: Option<(f64, f64)>,
    /// What the first transport failure said. A count alone reads as "the system
    /// refused"; the text is what distinguishes that from the LOAD GENERATOR
    /// running out of sockets, which is a fact about the harness.
    pub first_error: Option<String>,
    /// Microseconds, from the moment each request was DUE — not from when it was
    /// sent. A generator that falls behind and times from the send hides its own
    /// backlog, which is coordinated omission and the reason a saturated system
    /// can report healthy latency.
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

    /// Achieved rate over `secs`, which is the honest denominator: if it is below
    /// the requested rate the system did not keep up and the percentiles describe
    /// a system in backlog.
    pub fn rps(&self, secs: f64) -> f64 {
        self.ok as f64 / secs
    }
}

/// An open-loop run in flight.
pub struct OpenLoad {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<Report>>,
}

impl OpenLoad {
    pub fn stop(self) -> Report {
        self.stop.store(true, Ordering::Relaxed);
        let mut all =
            Report { due: 0, ok: 0, shed: 0, failed: 0, statuses: Default::default(), first_refusal: None, refusal_window: None, first_error: None, latencies: Vec::new() };
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

/// Find one of our binaries.
///
/// `CARGO_BIN_EXE_*` only exists inside an integration test, and this harness is used
/// from a benchmark binary too — so the lookup walks from wherever the current
/// executable is (a test lives in `target/release/deps/`, the bench in
/// `target/release/`) and falls back to the workspace path. An override exists for
/// the case where neither is true.
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
    repo_root().join(format!("reconciler/target/release/{name}"))
}

/// Where the specs and artifacts live.
///
/// `CARGO_MANIFEST_DIR` is baked in at COMPILE time, which is right for a test
/// running where it was built and useless for a benchmark cross-compiled to
/// another machine — the path it names does not exist there. The override is
/// what makes `bench/` able to drive a second box at all.
pub fn repo_root() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("COMP_REPO_ROOT") {
        return std::path::PathBuf::from(p);
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// Children write to a file rather than to /dev/null: several assertions are about
/// what a process SAID — the phase timings a host prints, the reason a reconciler
/// gives — and a test that cannot read them has to guess.
fn spawn_logged(name: &str, cmd: &mut Command, log: &std::path::Path) -> Kill {
    let f = std::fs::File::create(log).unwrap_or_else(|e| panic!("creating {}: {e}", log.display()));
    let err = f.try_clone().unwrap();
    Kill(
        cmd.stdout(Stdio::from(f))
            .stderr(Stdio::from(err))
            .spawn()
            .unwrap_or_else(|e| panic!("spawning {name}: {e}")),
    )
}

/// A port nothing is listening on, found by asking the OS.
///
/// This replaced ports derived from a hash of the lattice name, which collided:
/// `ha` with `autoscale` and `sharedstate` with `coldstart` both landed on the same
/// block, so those tests passed alone and failed whenever the suite ran in parallel —
/// which is how it is normally run. A name-derived port is a guess about a namespace
/// the OS already owns.
///
/// There is a race between closing this listener and the child binding it. It is
/// small, and the alternative — children reporting a port they chose — needs a
/// channel out of every process here, including `nats-server`.
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("no free port")
        .local_addr()
        .unwrap()
        .port()
}

impl Fleet {
    /// `specs` are authored YAML paths relative to `comp/`. `max_inflight` sets the
    /// ingress shedding bound; `None` leaves it at the default.
    pub fn start(lattice: &str, specs: &[&str], nodes: u16, max_inflight: Option<u32>) -> Self {
        Self::start_with_kv(lattice, specs, nodes, max_inflight, None)
    }

    /// `kv` picks the host's storage backend. `None` leaves the lattice default
    /// (`nats`, shared); `Some("sqlite")` gives every node its own file, which is the
    /// arrangement a spread stateful app must be refused on.
    pub fn start_with_kv(
        lattice: &str,
        specs: &[&str],
        nodes: u16,
        max_inflight: Option<u32>,
        kv: Option<&str>,
    ) -> Self {
        // Tests run what production runs: pooling on (ADR-0054), read cache off
        // (ADR-0063 — it trades cross-node freshness, so a test asserting shared
        // state must not get it by accident).
        Self::start_full(lattice, specs, &[], &[], nodes, max_inflight, kv, true, 0, &[], false)
    }

    /// A fleet whose control plane holds a vault: `vault://<org>/<name>=value`.
    ///
    /// A reference a spec grants and this list omits is the case worth having a
    /// harness for — that instance must never start (ADR-0051).
    pub fn start_with_secrets(
        lattice: &str,
        specs: &[&str],
        artifacts: &[String],
        secrets: &[String],
    ) -> Self {
        Self::start_full(lattice, specs, artifacts, secrets, 1, None, None, true, 0, &[], false)
    }

    /// A fleet driven by the REAL control plane, for tests that exercise the
    /// platform's own API rather than a fixture — deploying, then spawning an
    /// environment and watching the loop converge on it (ADR-0078).
    pub fn start_with_platform(lattice: &str, nodes: u16) -> Self {
        Self::start_full(lattice, &[], &[], &[], nodes, None, None, true, 0, &[], true)
    }

    /// Where the control plane is listening.
    pub fn platform_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.platform_port)
    }

    /// A fleet whose nodes carry LABELS, so placement constraints have something
    /// to match. `labels[i]` is applied to node `i+1`; a node past the end gets
    /// none.
    ///
    /// Without this a constrained app is simply unschedulable, and a test that
    /// wanted two components on two different nodes would quietly get them on one
    /// — proving nothing while passing.
    pub fn start_with_labels(
        lattice: &str,
        specs: &[&str],
        artifacts: &[String],
        labels: &[&str],
    ) -> Self {
        Self::start_labelled_kv(lattice, specs, artifacts, labels, None)
    }

    /// The same, choosing the store. `memory` is what a hop measurement wants:
    /// ADR-0057's lesson is that JetStream round trips dominate everything else,
    /// and a cross-node call hidden under them cannot be seen at all.
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
        )
    }

    /// Every node caches reads for `cache_ms` (ADR-0063). The interesting fleet is
    /// two or more: on one node the cache invalidates its own writes and cannot be
    /// caught being stale.
    pub fn start_with_cache(lattice: &str, specs: &[&str], nodes: u16, cache_ms: u64) -> Self {
        Self::start_full(lattice, specs, &[], &[], nodes, None, None, true, cache_ms, &[], false)
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
        // 0 leaves the read cache off, which is what every other entry point wants.
        cache_ms: u64,
        // `labels[i]` goes to node i+1, as `--label k=v`. Empty means unlabelled,
        // which is what every entry point but `start_with_labels` wants.
        labels: &[&str],
        // Run `platform-domain` as the control plane instead of `comp-stub`.
        real_platform: bool,
    ) -> Self {
        let root = repo_root();
        let host_bin = std::env::var("COMP_HOST_BIN")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| root.join("host/target/release/comp-host"));
        assert!(host_bin.exists(), "missing {} — cargo build --release in host/", host_bin.display());

        let (nats_port, platform_port, ingress_port) = (free_port(), free_port(), free_port());
        let dir = tempfile::tempdir().unwrap();
        let sp = dir.path().to_path_buf();
        let mut children = Vec::new();

        let mut nats = Command::new("nats-server");
        nats.args(["-js", "-sd"])
            .arg(sp.join("nats"))
            .args(["-a", "127.0.0.1", "-p", &nats_port.to_string()]);
        children.push(spawn_logged("nats-server", &mut nats, &sp.join("nats.log")));
        std::thread::sleep(Duration::from_secs(2));

        // The REAL control plane instead of the stub, when asked. `comp-stub` serves
        // fixtures and nothing else, which is right for a placement test and
        // useless for anything that exercises the platform's own API — spawning an
        // environment, for instance, is a `platform-domain` feature the stub has
        // never heard of.
        if real_platform {
            let component = root.join("components/target/platform_domain.composed.wasm");
            assert!(component.exists(), "missing {} — just compose-platform", component.display());
            let mut cp = Command::new(&host_bin);
            cp.current_dir(&root)
                .arg("--component")
                .arg(&component)
                .args(["--addr", &format!("127.0.0.1:{platform_port}"), "--kv", "sqlite"])
                .arg("--sqlite-path")
                .arg(sp.join("platform.db"))
                .args(["--tenant", "platform", "--app", "control-plane"])
                .args(["--config", "applier-secret=test-secret"])
                .args(["--config", "ingress-suffix=test"])
                // Envelope encryption for the vault (`secrets-vault`). Without
                // it every write is refused with "master key missing", which
                // makes the whole secret path untestable — and a fixed test key
                // is fine precisely because nothing real is ever stored here.
                .args(["--config", "master-key=Y29tcC10ZXN0LW9ubHktbWFzdGVyLWtleS0zMmJ5dGU="]);
            // Admission control (ADR-0082). A test that wants to see the refusal
            // sets it low; everything else needs it high enough not to fire.
            if let Ok(lag) = std::env::var("COMP_MAX_PLACEMENT_LAG") {
                cp.args(["--config", &format!("max-placement-lag={lag}")]);
            }
            // How old a fleet report may be before admission fails closed. Low
            // enough to observe, in the one test that wants to see it.
            if let Ok(per) = std::env::var("COMP_MAX_PLACEMENT_LAG_PER_NODE") {
                cp.args(["--config", &format!("max-placement-lag-per-node={per}")]);
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
            stub.args(["--artifact", "gate=components/target/gate_domain.composed.wasm"]);
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
                .args(["--lattice-nats", &nats_url, "--node", &format!("n{n}"), "--lattice", lattice])
                .args(["--addr", &format!("127.0.0.1:{host_port}")])
                .args(["--advertise-addr", &format!("127.0.0.1:{host_port}")])
                // Where a granted secret is fetched from (ADR-0051). Every node in a
                // real lattice has one; a harness that omitted it meant no test could
                // ever exercise the reader, which is how it went unwired.
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
            // A harness runs everything on loopback, and loopback is a PRIVATE
            // address the host refuses to dial by default (ADR-0008). So a test
            // whose subject talks to a real backing service — a database, say —
            // cannot exist without this, and it is opt-in rather than always-on
            // so that no OTHER test gets the allowance it never asked for.
            //
            // It widens the address check only. The per-instance allow-list still
            // decides which authority an instance may name, which is the half a
            // fixture is asserting when it writes `egress:` out by hand.
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
            .args(["--secret", "test-secret", "--nats-url", &nats_url, "--lattice", lattice])
            // A lease TTL near the interval, so a failover test does not spend a
            // minute waiting for what production would tune to 30s.
            .args(["--interval", "3", "--lease-ttl", "6"]);
        let rec_child = spawn_logged("comp-reconciler", &mut rec, &sp.join("rec.log"));
        let first_reconciler_pid = rec_child.0.id();
        children.push(rec_child);

        let mut ing = Command::new(bin_path("comp-ingress"));
        ing.current_dir(&root)
            .args(["--addr", &format!("127.0.0.1:{ingress_port}")])
            .args(["--nats-url", &nats_url, "--lattice", lattice, "--refresh-secs", "2"]);
        if let Some(m) = max_inflight {
            ing.args(["--max-inflight", &m.to_string()]);
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

    /// A SECOND ingress against the same lattice.
    ///
    /// It holds no state beyond a cache of inventory, so several should be able to
    /// run — "should" being the word the test using this exists to remove.
    pub fn second_ingress(&mut self) -> u16 {
        let port = free_port();
        let mut ing = Command::new(bin_path("comp-ingress"));
        ing.current_dir(repo_root())
            .args(["--addr", &format!("127.0.0.1:{port}")])
            .args(["--nats-url", &self.nats_url, "--lattice", &self.lattice, "--refresh-secs", "2"]);
        self._children.push(spawn_logged("comp-ingress-b", &mut ing, &self.dir.path().join("ingress-b.log")));
        port
    }

    /// Retry the FIRST REAL OPERATION until it works, and return what it
    /// returned.
    ///
    /// This exists because "is it ready yet" has been answered wrongly four times
    /// in this repo, in four different tests, each in the same way: a readiness
    /// probe chosen SEPARATELY from the thing being measured, which then proved
    /// something adjacent to it.
    ///
    ///   * `graph.rs` and `vgit.rs` polled the app's root route — which touches
    ///     no capability, so it answered before the link, the egress and the
    ///     database were usable, and the first real request lost the race.
    ///   * `ha.rs` used `serves()`, which an ingress satisfies through the
    ///     ACTIVATION path with an empty routing table — so it went green while
    ///     nothing was placed and nothing was routable.
    ///   * `fitness.rs` polled a call the evaluator refuses before making any
    ///     HTTP request, so it proved the component was reachable and said
    ///     nothing about the runner behind it.
    ///
    /// Every one of those passed alone and failed under load, which is the shape
    /// that gets dismissed as flakiness. Three of them were.
    ///
    /// The rule that removes the whole class: **do not have a separate readiness
    /// signal.** Retry the operation the test actually cares about. A probe
    /// cannot then prove the wrong thing, because there is no probe — and a
    /// fleet that is not ready simply makes the first call fail, which is what
    /// retrying is for.
    ///
    /// `f` returns `Err(why)` while it is not ready. The `why` is kept and
    /// printed on timeout beside the node and reconciler logs, because "never
    /// became ready" without the last error is the least useful failure there is.
    pub fn until<T>(
        &self,
        what: &str,
        within: Duration,
        mut f: impl FnMut() -> std::result::Result<T, String>,
    ) -> T {
        let deadline = Instant::now() + within;
        let mut last = "never attempted".to_string();
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

    /// Wait until the fleet has actually PLACED the app — until some node
    /// advertises an instance carrying `host` as its ingress.
    ///
    /// This exists because every cheaper check is a lie. A successful request
    /// proves nothing: an ingress with an empty routing table still answers, by
    /// asking the reconciler to activate the app and routing to whatever address
    /// comes back. So both `serves()` and "poll until requests stop failing" go
    /// green while inventory is still empty and no ingress can route anything —
    /// which is precisely how a test could pass in isolation, fail under load,
    /// and be misdiagnosed four times.
    ///
    /// Inventory is the only honest answer to "has it converged", because
    /// inventory is what routing is built from.
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
                        serde_json::from_slice::<serde_json::Value>(&comp_lattice::snapshot::expand(
                            e.value.clone(),
                        ))
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

    /// What an ingress said. `""` is the first one, any other name is a suffix —
    /// `"-b"` for the second.
    ///
    /// Exists because a test that asserts an ingress served nothing, and then
    /// prints only the count, has thrown away the one thing that explains it.
    pub fn ingress_log(&self, which: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(format!("ingress{which}.log")))
            .unwrap_or_else(|e| format!("(no ingress{which}.log: {e})"))
    }

    /// A SECOND reconciler against the same lattice and control plane.
    ///
    /// It should stand by rather than reconcile: exactly one holds the lease
    /// (ADR-0072). `n` names its log so a test can read which one did what.
    pub fn second_reconciler(&mut self, n: &str) -> &Kill {
        let mut rec = Command::new(bin_path("comp-reconciler"));
        rec.current_dir(repo_root())
            .args(["--platform-url", &format!("http://127.0.0.1:{}", self.platform_port)])
            .args(["--secret", "test-secret", "--nats-url", &self.nats_url])
            .args(["--lattice", &self.lattice, "--interval", "3", "--lease-ttl", "6"]);
        let k = spawn_logged(
            &format!("comp-reconciler-{n}"),
            &mut rec,
            &self.dir.path().join(format!("rec-{n}.log")),
        );
        self.reconcilers.push(k);
        self.reconcilers.last().unwrap()
    }

    /// What a named second reconciler said.
    pub fn reconciler_log_named(&self, n: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(format!("rec-{n}.log"))).unwrap_or_default()
    }

    /// Kill the reconciler started with `start`, leaving any standby running.
    ///
    /// By pid. A pattern match on the command line kills every reconciler on this
    /// lattice, because a standby is started with the same arguments by design —
    /// which is how the first version of this quietly killed both and made the
    /// takeover look broken.
    pub fn kill_first_reconciler(&mut self) {
        let _ = std::process::Command::new("kill")
            .args(["-9", &self.first_reconciler_pid.to_string()])
            .status();
    }

    /// Kill node `n` outright — SIGKILL, no chance to tidy up.
    ///
    /// The point is that it does NOT get to say goodbye. A host that deregisters
    /// on its way out exercises the polite path, which is not the one that
    /// happens when a machine loses power or the OOM killer arrives. What has to
    /// hold is that the lattice notices by itself: inventory expires, the
    /// reconciler sees a gap, and the work is placed somewhere else.
    ///
    /// Returns the pid, so a caller can say which one it took.
    pub fn kill_host(&self, n: u16) -> Option<u32> {
        let pid = *self.host_pids.get((n as usize).saturating_sub(1))?;
        let _ = std::process::Command::new("kill").args(["-9", &pid.to_string()]).status();
        Some(pid)
    }

    /// How many nodes this fleet started with.
    pub fn node_count(&self) -> usize {
        self.host_pids.len()
    }

    /// Stop whichever process was started last — used to kill an ingress and watch
    /// the other one carry on.
    pub fn kill_last(&mut self) {
        self._children.pop();
    }

    /// Which node answered, over `n` requests to `port`. The `x-comp-node` header is
    /// the only way to see the balance from outside.
    pub fn who_answers(&self, port: u16, n: usize) -> (std::collections::BTreeMap<String, usize>, usize) {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
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

    /// A fleet from a directory of specs and an explicit artifact list.
    ///
    /// The test entry points name one spec and one artifact because that is what a
    /// scenario needs; the matrix varies both, and sharing this constructor is what
    /// keeps a benchmark measuring the same fleet the tests assert on.
    pub fn start_bench(
        lattice: &str,
        spec_dir: &str,
        artifacts: &[String],
        nodes: u16,
        pool: bool,
        // Which storage backend the guests get. The benchmark component reads and
        // writes on every request, so this is not a detail: with `nats` each
        // request pays two JetStream round trips and the number measures the bus.
        kv: Option<&str>,
    ) -> Self {
        Self::start_full(lattice, &[spec_dir], artifacts, &[], nodes, None, kv, pool, 0, &[], false)
    }

    /// The host process for node `n`, so a caller can read its RSS.
    pub fn host_pid(&self, n: u16) -> Option<u32> {
        self.host_pids.get((n as usize).saturating_sub(1)).copied()
    }

    /// How many instances this node reports having started.
    pub fn started_count(&self) -> usize {
        self.node_log("n1").matches("comp-host: started ").count()
    }

    /// How each module arrived on node `n`: (shared, from disk, compiled).
    ///
    /// The distinction is the whole point of the digest cache, and reading it from
    /// the host's own log means the benchmark cannot disagree with the host about
    /// what happened.
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

    /// The control plane's own log, whichever control plane this fleet started.
    ///
    /// In `until`'s failure dump because a fleet that never places anything looks
    /// identical from the node and the reconciler whether the control plane
    /// refused a manifest or never started at all — and those are opposite bugs.
    /// Both names are read: `comp-stub` and `platform-domain` are alternatives,
    /// and reading only one produces an empty section that reads as "nothing
    /// wrong here".
    pub fn platform_log(&self) -> String {
        let stub = std::fs::read_to_string(self.dir.path().join("stub.log")).unwrap_or_default();
        let real = std::fs::read_to_string(self.dir.path().join("platform.log")).unwrap_or_default();
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

    /// Constant load until stopped, counting answers and refusals separately —
    /// a 503 from the ingress is not a failed request, it is the platform declining
    /// one, and conflating them is how a shed storm reads as an outage.
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

    /// Load at a fixed ARRIVAL RATE, whatever the system does with it.
    ///
    /// Each worker owns a slice of the schedule and sleeps until its next request
    /// is due; if the previous response has not come back by then it fires late
    /// and the lateness lands in the measurement, because that is what a real
    /// client experiences. Requests are timed from due, not from send.
    ///
    /// `port` picks the door: the ingress, or a host directly — the only way to
    /// find out what the extra hop costs.
    ///
    /// Every worker walks the WHOLE host list, one app per request, rather than
    /// pinning to one app for the run.
    ///
    /// Pinning meant `min(workers, apps)` apps ever saw traffic: the 200-app cell
    /// on the Pi had twelve workers, so 188 of the apps were idle and the cell
    /// measured one app's throughput with 199 spectators. Rotating is what makes
    /// "200 apps busy" a thing the harness can express at all.
    /// `rate: None` is a CLOSED loop — every worker sends again as soon as its
    /// own response lands. Then there is no schedule to be late against, so
    /// latency is timed from the send and the percentiles describe a system whose
    /// concurrency the harness pinned. Said out loud at the call site, because
    /// that is the mode whose mean is `workers / rps` and nothing more.
    pub fn open_load(
        &self,
        hosts: &[String],
        port: u16,
        rate: Option<u32>,
        workers: usize,
    ) -> OpenLoad {
        let stop = Arc::new(AtomicBool::new(false));
        let url = format!("http://127.0.0.1:{port}/api/ratelimit");
        let gap = rate.map(|r| {
            Duration::from_secs_f64(1.0 / (r as f64 / workers as f64).max(1.0))
        });
        let mut threads = Vec::new();
        for w in 0..workers {
            let (stop, url) = (stop.clone(), url.clone());
            // Offset per worker so they do not march through the list in step,
            // which would make every app hot and cold together instead of all of
            // them warm.
            let hosts = hosts.to_vec();
            let mut turn = w;
            threads.push(std::thread::spawn(move || {
                let client = reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()
                    .unwrap();
                let mut r =
                    Report { due: 0, ok: 0, shed: 0, failed: 0, statuses: Default::default(), first_refusal: None, refusal_window: None, first_error: None, latencies: Vec::new() };
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

    /// The HTTP port node `n` serves on, for load that skips the ingress.
    pub fn host_port(&self, n: u16) -> Option<u16> {
        self.host_ports.get((n as usize).saturating_sub(1)).copied()
    }

    /// Poll until a request to `host` is answered, or give up.
    pub fn serves(&self, host: &str, within: Duration) -> bool {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
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
