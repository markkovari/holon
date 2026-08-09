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

fn repo_root() -> std::path::PathBuf {
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
fn free_port() -> u16 {
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

        let mut stub = Command::new(env!("CARGO_BIN_EXE_comp-stub"));
        stub.current_dir(&root).args(["--port", &platform_port.to_string()]);
        for s in specs {
            stub.args(["--spec", s]);
        }
        stub.args([
            "--artifact",
            "gate=components/target/gate_domain.composed.wasm",
        ]);
        children.push(spawn_logged("comp-stub", &mut stub, &sp.join("stub.log")));

        let nats_url = format!("nats://127.0.0.1:{nats_port}");
        for n in 1..=nodes {
            let host_port = free_port();
            let mut c = Command::new(&host_bin);
            c.current_dir(&root)
                .args(["--lattice-nats", &nats_url, "--node", &format!("n{n}"), "--lattice", lattice])
                .args(["--addr", &format!("127.0.0.1:{host_port}")])
                .args(["--advertise-addr", &format!("127.0.0.1:{host_port}")])
                .arg("--state-dir")
                .arg(sp.join(format!("n{n}")));
            if let Some(kv) = kv {
                c.args(["--kv", kv]).arg("--sqlite-path").arg(sp.join(format!("n{n}/kv.db")));
            }
            children.push(spawn_logged("comp-host", &mut c, &sp.join(format!("n{n}.log"))));
        }
        std::thread::sleep(Duration::from_secs(2));

        let mut rec = Command::new(env!("CARGO_BIN_EXE_comp-reconciler"));
        rec.current_dir(&root)
            .args(["--platform-url", &format!("http://127.0.0.1:{platform_port}")])
            .args(["--secret", "test-secret", "--nats-url", &nats_url, "--lattice", lattice])
            .args(["--interval", "3"]);
        children.push(spawn_logged("comp-reconciler", &mut rec, &sp.join("rec.log")));

        let mut ing = Command::new(env!("CARGO_BIN_EXE_comp-ingress"));
        ing.current_dir(&root)
            .args(["--addr", &format!("127.0.0.1:{ingress_port}")])
            .args(["--nats-url", &nats_url, "--lattice", lattice, "--refresh-secs", "2"]);
        if let Some(m) = max_inflight {
            ing.args(["--max-inflight", &m.to_string()]);
        }
        children.push(spawn_logged("comp-ingress", &mut ing, &sp.join("ingress.log")));

        Self {
            dir,
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
        let mut ing = Command::new(env!("CARGO_BIN_EXE_comp-ingress"));
        ing.current_dir(repo_root())
            .args(["--addr", &format!("127.0.0.1:{port}")])
            .args(["--nats-url", &self.nats_url, "--lattice", &self.lattice, "--refresh-secs", "2"]);
        self._children.push(spawn_logged("comp-ingress-b", &mut ing, &self.dir.path().join("ingress-b.log")));
        port
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

    /// Replicas the fleet is running, straight from inventory.
    pub fn replicas(&self) -> u32 {
        let out = Command::new(env!("CARGO_BIN_EXE_comp-bench"))
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
