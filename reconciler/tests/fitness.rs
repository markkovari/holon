//! The gate, end to end: a component judges a candidate by running real commands.
//!
//! All JUDGEMENT (what is required, what a score means, what `need-base` implies) is in a component;
//! all PROCESS SPAWNING is in a native runner, reached over HTTP through an egress allow-list the
//! component did not write. The unit tests cannot reach whether the seam holds: the host letting it
//! dial out, the composer linking a non-`wasi` interface, a whole base tree surviving a chunked write
//! through the sandbox, and `need-base` coming back as its own answer rather than a rejection.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use comp_fleet::fleet::{bin_path, free_port, repo_root, Fleet};
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
    /// Started with NO checkout: the tree arrives from whoever is asking, not pinned to a machine.
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

    fn with_token(token_file: &std::path::Path) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let port = free_port();
        let child = Command::new(bin_path("comp-checks"))
            .args(["--addr", &format!("127.0.0.1:{port}")])
            .arg("--work-dir")
            .arg(dir.path())
            .arg("--token-file")
            .arg(token_file)
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
        assert!(p.exists(), "missing {} — run `cargo xtask build --force`", p.display());
        out.push(format!("{id}={}", p.display()));
    }
    out
}

fn spec_for(port: u16) -> std::path::PathBuf {
    spec_pointing_at(&format!("127.0.0.1:{port}"))
}

/// The manifest, pointed at any `host:port`, so a remote runner can be driven by the same test. The
/// authority lands in TWO places: `checks-url` (where the component dials) and `egress` (what the host
/// LETS it dial). Getting only the first right looks like the runner being down (ADR-0008).
fn spec_pointing_at(authority: &str) -> std::path::PathBuf {
    let src = repo_root().join("fixtures/fitness.yaml");
    let yaml = std::fs::read_to_string(&src).unwrap().replace("127.0.0.1:CHECKS_PORT", authority);
    let out =
        std::env::temp_dir().join(format!("comp-fitness-{}.yaml", authority.replace(':', "-")));
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

/// The first real evaluation, retried until it works. NOT a separate readiness probe (see
/// `Fleet::until`): an empty check list is refused before any HTTP, so polling it proved nothing about
/// the runner. A real check against a base nobody posted comes back `need-base`, known only by asking.
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
    // An EMPTY grant: this runner wants no token, spelled as a secret with nothing in it. The component
    // sends no header, keeping "no token" a covered path.
    let dir = tempfile::tempdir().unwrap();
    let none = dir.path().join("no-token");
    std::fs::write(&none, "").unwrap();
    let fleet = Fleet::start_with_secrets(
        "fitness",
        &[spec.to_str().unwrap()],
        &artifacts(),
        &[format!("vault://acme/checkstoken=@{}", none.display())],
    );
    let probe = wait_for_probe(&fleet);

    let commit = "1111111111111111111111111111111111111111";

    // --- the runner asks for a base it has not seen ---
    // Its own answer, not a rejection: a cold cache is not a bad candidate.
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

    // --- a candidate that passes everything ---
    // The base tree travels through the sandbox and a chunked write, which only this test exercises.
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

    // --- a candidate that does not, and is still ranked ---
    // The base is cached now, so this one sends only its diff, as every later candidate does.
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

    // --- the base was reused CLEAN ---
    // `half-way` wrote 41 over the base and `nothing` did not touch src/lib.rs; an accumulating cache
    // would have shown 42 to `the-fix`.
    let outcomes = worst["outcomes"].as_array().cloned().unwrap_or_default();
    let fix = outcomes.iter().find(|o| o["id"] == json!("the-fix")).unwrap();
    // `state`, not `passed`: the COMPONENT answers `graph:fitness`, a three-way state (passed / failed /
    // not-attempted). `passed` is `comp-checks`'s own wire shape one layer down and reads `null` here.
    assert_eq!(
        fix["state"],
        json!("failed"),
        "a candidate saw an earlier one's edit — a cached base that accumulates makes \
         every later score wrong: {worst}"
    );

    // --- an empty gate is refused rather than passed ---
    // The arithmetic would accept it vacuously, since no required check failed.
    let empty =
        probe.call("/evaluate", json!({ "name": "x", "base_commit": commit, "checks": [] }));
    assert_eq!(empty["error"], json!("invalid"), "an empty check list must be refused: {empty}");

    println!("    judged three candidates through a real runner: 1000, {mid}, {}", worst["score"]);
}

/// A runner behind a bearer token, reached through the component that was granted one, and refusing
/// the component granted the wrong one.
///
/// `comp-checks` refuses to listen beyond loopback without `--token-file`, so every remote gate is
/// authenticated. The credential must survive the whole path only this test covers: a manifest grant,
/// the host's vault, `comp:secrets/reader`, and an `authorization` header on a `wasi:http` request.
/// Loopback on purpose: the CREDENTIAL is under test, not the bind guard.
#[test]
fn a_gate_behind_a_token_is_reached_with_it_and_refused_without_it() {
    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let dir = tempfile::tempdir().unwrap();
    let right = dir.path().join("right");
    let wrong = dir.path().join("wrong");
    std::fs::write(&right, "the-real-token\n").unwrap();
    std::fs::write(&wrong, "yesterdays-token\n").unwrap();

    let checks = Checks::with_token(&right);
    let spec = spec_for(checks.port);
    let candidate = json!({
        "name": "c", "base_commit": "2222222222222222222222222222222222222222",
        "base_tree": [{ "path": "README", "content": "hello\n" }],
        "checks": [check("base", true, 1, &["test", "-f", "README"])],
    });

    {
        let fleet = Fleet::start_with_secrets(
            "fittoken",
            &[spec.to_str().unwrap()],
            &artifacts(),
            &[format!("vault://acme/checkstoken=@{}", right.display())],
        );
        let probe = wait_for_probe(&fleet);
        let r = probe.call("/evaluate", candidate.clone());
        assert_eq!(
            r["accepted"],
            json!(true),
            "the granted token did not reach the runner — the header is built inside \
             the sandbox and this is the only test that proves it arrives: {r}"
        );
    }

    // --- granted the wrong one ---
    // The message matters more than the status: a rotated token looks like a broken gate, and
    // `unavailable` would send the reader to the network.
    {
        let fleet = Fleet::start_with_secrets(
            "fitnotoken",
            &[spec.to_str().unwrap()],
            &artifacts(),
            &[format!("vault://acme/checkstoken=@{}", wrong.display())],
        );
        // `wait_for_probe` expects `need-base`; with a bad token it never gets that far, so this waits
        // on the refusal itself.
        let probe = Probe {
            port: fleet.ingress_port,
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .unwrap(),
        };
        let mut last = Value::Null;
        fleet.until("the runner's refusal", Duration::from_secs(120), || {
            last = probe.call("/evaluate", candidate.clone());
            if last["error"] == json!("invalid") {
                Ok(())
            } else {
                Err(last.to_string())
            }
        });
        let detail = last["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains("checks-token"),
            "the refusal has to name the grant that fixes it: {last}"
        );
        assert!(
            last["error"] != json!("unavailable"),
            "a rejected token is not an unreachable runner — that reads as a network \
             fault and sends whoever gets it to the wrong half: {last}"
        );
    }
}

/// The gate on ANOTHER MACHINE, judged through the component that dials it.
///
/// Ignored by default: it needs a `comp-checks` listening somewhere reachable, which CI has not. It
/// exercises the last untested hop: a `wasi:http` call to a non-loopback host through an egress
/// allow-list naming a real machine.
///
///   COMP_REMOTE_CHECKS=100.64.0.1:8199 \
///   COMP_REMOTE_TOKEN=~/.comp-secrets/checks \
///   cargo test --release --test fitness -- --ignored remote
#[test]
#[ignore = "needs a comp-checks on another machine; see the doc comment"]
fn a_gate_on_another_machine_judges_a_candidate() {
    let authority = std::env::var("COMP_REMOTE_CHECKS")
        .expect("set COMP_REMOTE_CHECKS=host:port to the runner this should use");
    let token = std::env::var("COMP_REMOTE_TOKEN")
        .expect("set COMP_REMOTE_TOKEN to the FILE holding that runner's token");
    let token = shellexpand(&token);

    // A tailnet address is PRIVATE and egress refuses those by default, hence this one line.
    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let spec = spec_pointing_at(&authority);
    let fleet = Fleet::start_with_secrets(
        "fitremote",
        &[spec.to_str().unwrap()],
        &artifacts(),
        &[format!("vault://acme/checkstoken=@{token}")],
    );
    let probe = wait_for_probe(&fleet);

    let commit = "bbbb000000000000000000000000000000000002";
    let tree = json!([
        { "path": "src/lib.py", "content": "def answer():\n    return 41\n" },
        { "path": "test.py", "content":
            "import sys; sys.path.insert(0,'src')\nfrom lib import answer\nassert answer() == 42, answer()\nprint('ok')\n" },
    ]);
    let checks = json!([
        check("tree-arrived", false, 1, &["test", "-f", "src/lib.py"]),
        check("tests-pass", true, 1, &["python3", "test.py"]),
    ]);

    // The candidate that changes nothing, against a base that fails on purpose.
    let r = probe.call(
        "/evaluate",
        json!({ "name": "does-nothing", "base_commit": commit,
                "base_tree": tree, "changes": [], "checks": checks }),
    );
    assert_eq!(r["accepted"], json!(false), "the broken base was accepted: {r}");
    let failed =
        r["outcomes"].as_array().unwrap().iter().find(|o| o["id"] == json!("tests-pass")).unwrap();
    assert_eq!(failed["state"], json!("failed"), "{r}");
    assert!(
        failed["detail"].as_str().unwrap_or_default().contains("AssertionError"),
        "the failure has to come back from the OTHER machine's filesystem, or this \
         proved nothing about where the work happened: {r}"
    );

    // The fix, with NO tree: the other machine cached it by commit, so a generation need not resend the repo.
    let r = probe.call(
        "/evaluate",
        json!({ "name": "the-fix", "base_commit": commit,
                "changes": [{ "path": "src/lib.py", "content": "def answer():\n    return 42\n" }],
                "checks": checks }),
    );
    assert_eq!(r["accepted"], json!(true), "the fix was rejected: {r}");
    assert_eq!(r["score"], json!(1000), "{r}");
}

/// `~` is not expanded by anything here, and a token path is where somebody writes one.
fn shellexpand(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
        None => p.to_string(),
    }
}
