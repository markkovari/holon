//! The gate, end to end: a component judges a candidate by running real commands.
//!
//! This is the seam the whole design of the gate rests on. All of the JUDGEMENT
//! is in a component — what is required, what a score means, what `need-base`
//! implies — and all of the PROCESS SPAWNING is in a native runner, because a
//! component cannot spawn a process and should not pretend to. The component
//! reaches it exactly the way it reaches a database or a model provider: over
//! HTTP, through an egress allow-list it did not write.
//!
//! What the unit tests on either side cannot reach is whether the seam holds:
//! whether the host lets it dial out, whether the composer links a non-`wasi`
//! interface, whether a whole base tree survives a chunked write through the
//! sandbox, and whether `need-base` comes back as its own answer rather than as a
//! rejection of the candidate.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use comp_reconciler::fleet::{bin_path, free_port, repo_root, Fleet};
use serde_json::{json, Value};

/// A native check runner that dies with the test.
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
    /// Started with NO checkout: the tree arrives from whoever is asking, which
    /// is the shape that is not pinned to a machine holding the repository.
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
    for (id, file) in [("gate", "fitness_probe.wasm"), ("fitness", "checks_runner.wasm")] {
        let p = dir.join(file);
        assert!(p.exists(), "missing {} — run `just build`", p.display());
        out.push(format!("{id}={}", p.display()));
    }
    out
}

fn spec_for(port: u16) -> std::path::PathBuf {
    let src = repo_root().join("fixtures/fitness.yaml");
    let yaml = std::fs::read_to_string(&src).unwrap().replace("CHECKS_PORT", &port.to_string());
    let out = std::env::temp_dir().join(format!("comp-fitness-{port}.yaml"));
    std::fs::write(&out, yaml).unwrap();
    out
}

struct Probe {
    port: u16,
    http: reqwest::blocking::Client,
}

impl Probe {
    fn call(&self, path: &str, body: Value) -> Value {
        let r = match self
            .http
            .post(format!("http://127.0.0.1:{}{path}", self.port))
            .header("host", "fitness.acme.test")
            .body(body.to_string())
            .send()
        {
            Ok(r) => r,
            Err(e) => return Value::String(format!("transport: {e}")),
        };
        let (status, text) = (r.status(), r.text().unwrap_or_default());
        serde_json::from_str(&text)
            .unwrap_or_else(|_| Value::String(format!("HTTP {status}: {text}")))
    }
}

/// The first real evaluation, retried until it works.
///
/// NOT a separate readiness probe — see `Fleet::until`. An empty check list is
/// refused by the evaluator before any HTTP happens, so polling THAT proved the
/// component was reachable and said nothing about the runner behind it: it went
/// green while the egress was still unusable and the next call timed out.
///
/// A real check against a base nobody has posted comes back `need-base`, which
/// can only be known by asking the runner.
fn wait_for_probe(fleet: &Fleet) -> Probe {
    let probe = Probe {
        port: fleet.ingress_port,
        http: reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap(),
    };
    fleet.until("an evaluation that reaches the runner", Duration::from_secs(120), || {
        let r = probe.call(
            "/evaluate",
            json!({
                "name": "ready", "base_commit": "0000000000000000000000000000000000000000",
                "checks": [{ "id": "r", "required": true, "weight": 1, "command": ["test", "-e", "."] }],
            }),
        );
        if r["error"] == json!("need-base") {
            Ok(())
        } else {
            Err(r.to_string())
        }
    });
    probe
}

fn check(id: &str, required: bool, weight: u32, command: &[&str]) -> Value {
    json!({
        "id": id, "required": required, "weight": weight,
        "command": command.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    })
}

#[test]
fn a_component_judges_a_candidate_by_running_real_commands() {
    let checks = Checks::start();
    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let spec = spec_for(checks.port);
    let fleet = Fleet::start_with_secrets("fitness", &[spec.to_str().unwrap()], &artifacts(), &[]);
    let probe = wait_for_probe(&fleet);

    let commit = "1111111111111111111111111111111111111111";

    // --- the runner asks for a base it has not seen --------------------------
    // Its own answer, not a rejection: a cold cache is not a bad candidate, and
    // collapsing the two would make the first evaluation of every run look like a
    // failure.
    let r = probe.call(
        "/evaluate",
        json!({
            "name": "first", "base_commit": commit,
            "checks": [check("base", true, 1, &["test", "-f", "README"])],
        }),
    );
    assert_eq!(
        r["error"],
        json!("need-base"),
        "an unknown base must come back as need-base, not as a rejected candidate: {r}"
    );

    // --- a candidate that passes everything ----------------------------------
    // The base tree travels through the sandbox and a chunked write on the way to
    // the runner, which is a thing only this test exercises.
    let base_tree = json!([
        { "path": "README", "content": "a project\n" },
        { "path": "src/lib.rs", "content": "pub fn answer() -> u32 { 41 }\n" },
    ]);
    let good = probe.call(
        "/evaluate",
        json!({
            "name": "fixes-it", "base_commit": commit, "base_tree": base_tree,
            "changes": [{ "path": "src/lib.rs", "content": "pub fn answer() -> u32 { 42 }\n" }],
            "checks": [
                check("base-arrived", true, 1, &["test", "-f", "README"]),
                check("the-fix", true, 1, &["grep", "-q", "42", "src/lib.rs"]),
            ],
        }),
    );
    assert_eq!(good["accepted"], json!(true), "a candidate that fixes it should pass: {good}");
    assert_eq!(good["score"], json!(1000), "and score full marks: {good}");

    // --- a candidate that does not, and is still ranked ----------------------
    // The base is cached now, so this one sends only its diff — which is what
    // every candidate after the first does in a real generation.
    let partial = probe.call(
        "/evaluate",
        json!({
            "name": "half-way", "base_commit": commit,
            "changes": [{ "path": "src/lib.rs", "content": "pub fn answer() -> u32 { 41 }\n" }],
            "checks": [
                check("base-arrived", true, 1, &["test", "-f", "README"]),
                check("the-fix", true, 1, &["grep", "-q", "42", "src/lib.rs"]),
            ],
        }),
    );
    assert_eq!(partial["accepted"], json!(false), "it did not fix it: {partial}");
    let mid = partial["score"].as_u64().unwrap();
    assert!(mid > 0 && mid < 1000, "half the checks is half the score: {partial}");

    // --- and one that does nothing at all ------------------------------------
    let worst = probe.call(
        "/evaluate",
        json!({
            "name": "nothing", "base_commit": commit,
            "changes": [{ "path": "notes.md", "content": "thinking about it\n" }],
            "checks": [
                check("base-arrived", false, 1, &["test", "-f", "README"]),
                check("the-fix", true, 1, &["grep", "-q", "42", "src/lib.rs"]),
                check("also-this", true, 1, &["test", "-f", "never.txt"]),
            ],
        }),
    );
    assert_eq!(worst["accepted"], json!(false));
    assert!(
        worst["score"].as_u64().unwrap() < mid,
        "a candidate that fixed nothing must rank below one that fixed half — that \
         ordering is the entire selection signal in a generation where nothing is \
         acceptable yet: {} vs {mid}",
        worst["score"]
    );

    // --- the base was reused CLEAN -------------------------------------------
    // `half-way` wrote 41 over the base and `nothing` did not touch src/lib.rs.
    // If the cached base had accumulated the previous candidate's file, `the-fix`
    // would have found 42 here.
    let outcomes = worst["outcomes"].as_array().cloned().unwrap_or_default();
    let fix = outcomes.iter().find(|o| o["id"] == json!("the-fix")).unwrap();
    assert_eq!(
        fix["passed"],
        json!(false),
        "a candidate saw an earlier one's edit — a cached base that accumulates makes \
         every later score wrong: {worst}"
    );

    // --- an empty gate is refused rather than passed -------------------------
    // The arithmetic would call it accepted, vacuously, since no required check
    // failed. That is how a swarm accepts everything.
    let empty =
        probe.call("/evaluate", json!({ "name": "x", "base_commit": commit, "checks": [] }));
    assert_eq!(empty["error"], json!("invalid"), "an empty check list must be refused: {empty}");

    println!("    judged three candidates through a real runner: 1000, {mid}, {}", worst["score"]);
}
