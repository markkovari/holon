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

    /// The same runner, behind a bearer token.
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
        assert!(p.exists(), "missing {} — run `just build`", p.display());
        out.push(format!("{id}={}", p.display()));
    }
    out
}

fn spec_for(port: u16) -> std::path::PathBuf {
    spec_pointing_at(&format!("127.0.0.1:{port}"))
}

/// The manifest, pointed at any `host:port`.
///
/// Split out so a remote runner can be driven by the same test that drives a
/// local one. The authority lands in TWO places and both matter: `checks-url`,
/// which is where the component dials, and `egress`, which is what the host will
/// LET it dial. Getting only the first right is the failure that looks like the
/// runner being down (ADR-0008).
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
    // An EMPTY grant: this runner wants no token, and that is spelled as a secret
    // with nothing in it rather than as a second manifest. The component filters
    // an empty value out and sends no header, which is what this whole test then
    // exercises — so "no token" stays a covered path rather than a missing one.
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
    // `state`, not `passed`. The COMPONENT answers `graph:fitness`, whose outcome
    // is a three-way state — passed / failed / not-attempted — because a check
    // that never ran because its dependency failed is a different fact from one
    // that ran and failed. `passed` is `comp-checks`'s own wire shape, one layer
    // down, and reading it here got `null`, which is not `false` and failed this
    // assertion for a candidate that behaved correctly.
    assert_eq!(
        fix["state"],
        json!("failed"),
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

/// A runner behind a bearer token, reached through the component that was granted
/// one — and the same runner refusing the component that was granted the wrong one.
///
/// This is the seam that makes the gate a SECOND MACHINE's job. `comp-checks`
/// refuses to listen anywhere but loopback without `--token-file`, because
/// `--allow` bounds the command and not the tree it runs over. So every remote
/// gate is an authenticated one, and the credential has to survive the whole path
/// this test covers and nothing else does: a manifest grant, the host's vault,
/// `comp:secrets/reader` inside the sandbox, and an `authorization` header on a
/// `wasi:http` request the component builds itself.
///
/// Loopback here on purpose. What is under test is the CREDENTIAL, not the bind
/// guard — and a test that needed a routable address would not run in CI.
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

    // --- granted the token the runner wants ----------------------------------
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

    // --- granted the wrong one -----------------------------------------------
    // The message matters more than the status. A rotated token looks exactly
    // like a broken gate from the outside, and `unavailable` would send whoever
    // reads it to look at the network — the wrong half of the system.
    {
        let fleet = Fleet::start_with_secrets(
            "fitnotoken",
            &[spec.to_str().unwrap()],
            &artifacts(),
            &[format!("vault://acme/checkstoken=@{}", wrong.display())],
        );
        // `wait_for_probe` asks for a base the runner does not have and expects
        // `need-base`; with a bad token it never gets that far, so this waits on
        // the refusal itself.
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
/// Ignored by default: it needs a `comp-checks` already listening somewhere this
/// machine can reach, which CI has not got. It is here because it is the only
/// thing that exercises the last untested hop — a `wasi:http` call out of the
/// sandbox to a host that is not loopback, through an egress allow-list naming
/// a real machine.
///
///   COMP_REMOTE_CHECKS=100.111.200.86:8199 \
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

    // A tailnet address is a PRIVATE address, and egress refuses those by default
    // — which is the correct default and the reason this is one line rather than
    // a surprise.
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

    // The fix, with NO tree: the other machine cached it by commit, which is what
    // stops a generation from sending the same repository once per candidate.
    let r = probe.call(
        "/evaluate",
        json!({ "name": "the-fix", "base_commit": commit,
                "changes": [{ "path": "src/lib.py", "content": "def answer():\n    return 42\n" }],
                "checks": checks }),
    );
    assert_eq!(r["accepted"], json!(true), "the fix was rejected: {r}");
    assert_eq!(r["score"], json!(1000), "{r}");
}

/// `~` is not expanded by anything here, and a token path is exactly where
/// somebody writes one.
fn shellexpand(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
        None => p.to_string(),
    }
}
