//! A search across generations, built so no single generation can succeed.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use comp_fleet::fleet::{bin_path, free_port, repo_root, Fleet};
use comp_goal::generation::{search, Bounds, SearchStop};
use serde_json::{json, Value};

struct Checks {
    child: Child,
    port: u16,
    _dir: tempfile::TempDir,
}
impl Drop for Checks {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Checks {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let port = free_port();
        let child = Command::new(bin_path("comp-checks"))
            .args(["--addr", &format!("127.0.0.1:{port}")])
            .arg("--work-dir")
            .arg(dir.path())
            .args(["--allow", "test", "--allow", "grep", "--timeout", "30"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("comp-checks — run `cargo build --release` in reconciler/");
        let me = Self { child, port, _dir: dir };
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return me;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("comp-checks never listened");
    }
}

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let mut out = Vec::new();
    for (id, file) in [
        ("sdprobe", "driver_probe.wasm"),
        ("sdriver", "agent_driver.wasm"),
        ("sagent", "agent_writer.wasm"),
        ("sllm", "mock_provider.wasm"),
        ("schecks", "checks_runner.wasm"),
    ] {
        let p = dir.join(file);
        assert!(p.exists(), "missing {} — run `cargo xtask build --force`", p.display());
        out.push(format!("{id}={}", p.display()));
    }
    out
}

fn spec_for(port: u16) -> std::path::PathBuf {
    let yaml = std::fs::read_to_string(repo_root().join("fixtures/search.yaml"))
        .unwrap()
        .replace("CHECKS_PORT", &port.to_string());
    let out = std::env::temp_dir().join(format!("comp-search-{port}.yaml"));
    std::fs::write(&out, yaml).unwrap();
    out
}

/// No way for one attempt to satisfy both required checks.
fn plan(text: &str) -> Value {
    json!({
        "text": text,
        "writable": ["src/lib.rs"],
        "context": [{ "path": "src/lib.rs", "content": "pub fn nothing() {}" }],
        "previous": [],
        "checks": [
            { "id": "has-one", "required": true, "weight": 1, "command": ["grep", "-q", "step_one", "src/lib.rs"] },
            { "id": "has-two", "required": true, "weight": 1, "command": ["grep", "-q", "step_two", "src/lib.rs"] },
        ],
        "base_commit": "4444444444444444444444444444444444444444",
        "base_tree": [
            { "path": "README", "content": "a project\n" },
            { "path": "src/lib.rs", "content": "pub fn nothing() {}\n" },
        ],
                // One: a self-repairing branch would make this a test of the driver (`driver.rs`).
        "max_attempts": 1,
        "seed": 0,
    })
}

const BRANCHES: u16 = 2;
const SEED: u64 = 5000;

fn body_of(files: &Value) -> String {
    files[0]["content"].as_str().unwrap_or_default().to_string()
}

#[test]
fn a_second_generation_builds_on_the_first_and_one_branch_refuses_to() {
    let checks = Checks::start();
    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let spec = spec_for(checks.port);
    let fleet = Fleet::start_with_secrets("search", &[spec.to_str().unwrap()], &artifacts(), &[]);

    let url = format!("http://127.0.0.1:{}/run", fleet.ingress_port);
    let host = "searchdrive.acme.test";
    let timeout = Duration::from_secs(120);

    let bounds = Bounds { branches: BRANCHES, max_rounds: 4, max_tokens: 0, patience: 0 };

    // Proven by a real search, not a readiness signal.
    fleet.until("the fleet serving a search", Duration::from_secs(180), || {
        let s = search(
            &url,
            host,
            &plan("go nowhere"),
            Bounds { max_rounds: 1, ..bounds },
            9000,
            timeout,
        );
        match s.rounds[0].entries.iter().find(|e| !e.note.is_empty()) {
            Some(e) => Err(e.note.clone()),
            None => Ok(()),
        }
    });

    let found = search(&url, host, &plan("grow it"), bounds, SEED, timeout);
    for (r, round) in found.rounds.iter().enumerate() {
        for e in &round.entries {
            println!(
                "    round {r} {:<9} accepted={:<5} score={:<5} {}",
                e.branch,
                e.accepted,
                e.score,
                body_of(&e.files).replace('\n', " ; ")
            );
        }
    }

    assert_eq!(found.stopped, SearchStop::Accepted, "the search found nothing: {found:?}");
    assert_eq!(found.rounds.len(), 2, "it should take exactly two generations: {found:?}");

    // `max_attempts` 1 and a winning rule keyed on a previous winner: the search did the work.
    assert!(
        found.rounds[0].entries.iter().all(|e| !e.accepted),
        "the first generation passed the gate, so this proves nothing about seeding: {found:?}"
    );

    let best = found.best.clone().expect("accepted, but no candidate");
    let body = body_of(&best.files);
    assert!(
        body.contains("step_one") && body.contains("step_two"),
        "the winner did not build on the first generation: {body}"
    );

    // Asserted by what it produced, not the flag.
    let blind = &found.rounds[1].entries[BRANCHES as usize - 1];
    assert!(
        !body_of(&blind.files).contains("step_one"),
        "the branch that reads nothing was shown the previous winner after all: {blind:?}"
    );
    assert!(
        found.rounds[1].entries[0].accepted,
        "and the branch that DOES read the winner is the one that finished it: {found:?}"
    );

    let summed: u64 =
        found.rounds.iter().flat_map(|r| r.entries.iter()).map(|e| e.spent_tokens).sum();
    assert_eq!(found.spent_tokens, summed, "a search's cost is every branch of every round");
    assert!(summed > 0, "nothing was spent, so nothing was asked: {found:?}");

    // Per-branch budgets can sum past a project's, hence a separate bound.
    let first_round: u64 = found.rounds[0].entries.iter().map(|e| e.spent_tokens).sum();
    let broke = search(
        &url,
        host,
        &plan("grow it"),
        Bounds { max_tokens: first_round, ..bounds },
        SEED,
        timeout,
    );
    assert_eq!(broke.stopped, SearchStop::OverBudget, "{broke:?}");
    assert_eq!(
        broke.rounds.len(),
        1,
        "a budget of one generation's worth must stop after one, having overshot by none: {broke:?}"
    );

    // The best score never improves: four rounds allowed, two spent.
    let circling =
        search(&url, host, &plan("go nowhere"), Bounds { patience: 1, ..bounds }, SEED, timeout);
    assert_eq!(circling.stopped, SearchStop::NoProgress, "{circling:?}");
    assert_eq!(
        circling.rounds.len(),
        2,
        "one generation that failed to improve must end it, out of a budget of four: {circling:?}"
    );

    println!(
        "    accepted in generation 2 of 4; over-budget after 1; no-progress after 2; \
         {} tokens across the search",
        found.spent_tokens
    );
}
