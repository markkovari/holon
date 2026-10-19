//! A decomposed goal, end to end: two parts, a contract, a negotiation, and one joined tree judged by
//! the same gate that judged the halves (ADR-0086).
//!
//! Eleven components across three apps, a real SurrealDB, a real native checks runner and a scripted
//! model, so ORCHESTRATION is asserted and not a model's mood.
//!
//! **The frontend cannot go green unless the negotiation actually happened.** Its script has two
//! rules: one writes a request when told to render results, one writes working code when it sees
//! `total_pages`, which is not in the first contract. A passing frontend means the request was read
//! from its candidate, answered by a second model call, written to the registry, ratified by the
//! backend's gate, and laid into the next prompt as `CONTRACT.md`.
//!
//! Skipped, loudly, when Docker cannot start the database.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use comp_fleet::fleet::{bin_path, free_port, repo_root, Fleet};
use comp_goal::compose;
use comp_goal::contract::{Answerer, Registry};
use comp_goal::generation::{Bounds, Part};
use serde_json::{json, Value};

// The shared test module of the root's suites, included rather than copied (docs/adr/0115).
#[path = "../../tests/harness/mod.rs"]
mod harness;
use harness::{Surreal, SURREAL_IMAGE, SURREAL_PASSWORD};

/// The commit the base tree is cached under. Not empty: the runner caches a posted tree by commit and
/// refuses to guess, and an empty one made every check fail with `need the tree for (no commit given)`.
const BASE: &str = "compose-base-1";

/// The interface the human wrote. Note what is NOT in it: `total_pages`.
const CONTRACT_V1: &str =
    r#"{"routes":[{"method":"GET","path":"/api/search","example":{"hits":[],"has_more":false}}]}"#;

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let mut out = Vec::new();
    for (id, file) in [
        ("probe", "driver_probe.wasm"),
        ("driver", "agent_driver.wasm"),
        ("agent", "agent_writer.wasm"),
        ("llm", "mock_provider.wasm"),
        ("gate", "checks_runner.wasm"),
        ("cprobe", "contract_probe.wasm"),
        ("registry", "contract_registry.wasm"),
        ("cgraph", "knowledge_graph.wasm"),
        ("lprobe", "llm_probe.wasm"),
        ("allm", "mock_provider.wasm"),
        ("mprobe", "memory_probe.wasm"),
        ("memory", "knowledge_memory.wasm"),
        ("graph", "knowledge_graph.wasm"),
        ("search", "search_index.wasm"),
        ("mllm", "mock_provider.wasm"),
    ] {
        let p = dir.join(file);
        assert!(p.exists(), "missing {} — run `cargo xtask build --force`", p.display());
        out.push(format!("{id}={}", p.display()));
    }
    out
}

/// The gate, as a real process: the runner that judges the branches also judges the composition, or
/// failures of the join become arguments about the harness.
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
            .stderr(Stdio::inherit())
            .spawn()
            .expect("comp-checks — run `cargo build --release` in reconciler/");
        let me = Self { child, port, _dir: dir };
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if std::net::TcpStream::connect(("127.0.0.1", me.port)).is_ok() {
                return me;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("comp-checks never listened");
    }
}

fn specs(surreal_port: u16, checks_port: u16) -> Vec<std::path::PathBuf> {
    [
        ("compose-driver.yaml", vec![("CHECKS_PORT", checks_port.to_string())]),
        ("compose-contract.yaml", vec![("SURREAL_PORT", surreal_port.to_string())]),
        ("compose-answer.yaml", vec![]),
        ("knowledge-memory.yaml", vec![("SURREAL_PORT", surreal_port.to_string())]),
    ]
    .into_iter()
    .map(|(name, subs)| {
        let mut yaml = std::fs::read_to_string(repo_root().join("fixtures").join(name)).unwrap();
        for (k, v) in subs {
            yaml = yaml.replace(k, &v);
        }
        let out = std::env::temp_dir().join(format!("comp-{name}-{surreal_port}"));
        std::fs::write(&out, yaml).unwrap();
        out
    })
    .collect()
}

fn base_tree() -> Value {
    json!([
        { "path": "README", "content": "one application, two halves\n" },
    ])
}

fn check(id: &str, command: &[&str]) -> Value {
    json!({ "id": id, "required": true, "weight": 1, "command": command })
}

/// A part's plan. `writable` is disjoint per part on purpose (`compose::merge` refuses an overlap).
fn part(name: &str, text: &str, writable: &[&str], checks: Value) -> Part {
    Part {
        name: name.into(),
        plan: json!({
            "text": text,
            "writable": writable,
            "context": base_tree(),
            "previous": [],
            "checks": checks,
            "base_commit": BASE,
            "base_tree": base_tree(),
            "max_attempts": 2,
            "seed": 1,
        }),
    }
}

#[test]
fn two_parts_negotiate_a_contract_and_land_one_joined_tree() {
    let Some(db) = Surreal::start() else {
        eprintln!(
            "SKIPPED: could not start {SURREAL_IMAGE} — this test needs a real database and \
             Docker to run it in. Nothing about the decomposed loop was verified by this run."
        );
        return;
    };
    let checks = Checks::start();

    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let spec_files = specs(db.port, checks.port);
    let spec_refs: Vec<&str> = spec_files.iter().map(|p| p.to_str().unwrap()).collect();
    let fleet = Fleet::start_with_secrets(
        "compose",
        &spec_refs,
        &artifacts(),
        &[format!("vault://acme/surreal={SURREAL_PASSWORD}")],
    );
    let port = fleet.ingress_port;

    let registry = Registry {
        url: format!("http://127.0.0.1:{port}"),
        host: "composecontract.acme.test".into(),
        timeout: Duration::from_secs(30),
    };
    let pool = comp_goal::memory::Memory {
        url: format!("http://127.0.0.1:{port}"),
        host: "memory.acme.test".into(),
        timeout: Duration::from_secs(30),
    };
    let answerer = Answerer {
        url: format!("http://127.0.0.1:{port}"),
        host: "composeanswer.acme.test".into(),
        timeout: Duration::from_secs(60),
    };

    // --- the human's contract ---
    // Retried until it lands: the first call touching SurrealDB through two components is also the
    // readiness check.
    let mut published = 0;
    fleet.until("publishing the first contract", Duration::from_secs(180), || {
        match registry.publish(CONTRACT_V1) {
            Ok(v) => {
                published = v;
                Ok(())
            }
            Err(e) => Err(e),
        }
    });
    assert_eq!(published, 1, "the human's contract is v1");
    let v1 = registry.current().expect("the contract reads back");
    assert!(v1.canonical, "the human's contract is canonical on arrival");
    assert!(!v1.body.contains("total_pages"), "the thing being negotiated is not in it yet");

    let parts = vec![
        part(
            "backend",
            "serve the search route",
            &["src/api.rs"],
            json!([check("route-exists", &["grep", "-q", "/api/search", "src/api.rs"])]),
        ),
        part(
            "frontend",
            "render the results with a pager",
            &["ui/app.ts", compose::REQUEST_PATH],
            // Neither the id NOR THE COMMAND may contain the trigger string: a repair prompt carries
            // the failing check's command, so `grep -q total_pages` leaked the answer and the frontend
            // passed in round one. The scripted model matches on everything the caller said, as a real
            // model would read it.
            json!([check("pager-renders", &["grep", "-q", "pager", "ui/app.ts"])]),
        ),
    ];

    // --- the run, through the SAME code `comp goal run` uses ---
    // `compose::run_parts` is the orchestration; the binary only prints and lands.
    let run = compose::run_parts(
        &compose::Wiring {
            driver_url: &format!("http://127.0.0.1:{port}/run"),
            driver_host: "composedrive.acme.test",
            checks_url: &format!("http://127.0.0.1:{}/check", checks.port),
            // This harness starts its own runner on loopback with no token.
            checks_token: None,
            registry: &registry,
            answerer: Some(&answerer),
            // The parts read, write and attribute against a real pool, covering the decomposed path.
            memory: Some(&pool),
        },
        &parts,
        &v1.body,
        v1.number,
        Bounds { branches: 1, max_rounds: 3, max_tokens: 0, patience: 0 },
        7_000,
        Duration::from_secs(120),
        BASE,
        &base_tree(),
        // The goal's own checks, belonging to neither part: the frontend reads a field the backend
        // serves, which only the joined tree can show.
        &json!([
            check("both-halves-present", &["test", "-f", "src/api.rs"]),
            check("the-join", &["grep", "-q", "total_pages", "ui/app.ts"]),
            check("backend-serves-it", &["grep", "-q", "total_pages", "src/api.rs"]),
        ]),
    );

    for line in &run.log {
        println!("  · {line}");
    }
    for p in &run.composition.parts {
        println!(
            "  {} accepted={} score={} rounds={} against v{}",
            p.part,
            p.accepted,
            p.best.as_ref().map(|b| b.score).unwrap_or(0),
            p.rounds.len(),
            p.built_against
        );
    }

    assert!(
        run.log.iter().any(|l| l.contains("asked") && l.contains("total_pages")),
        "the frontend never asked for anything — the request channel is broken: {:?}",
        run.log
    );
    assert!(
        run.log.iter().any(|l| l.contains("granted")),
        "nothing answered the request: {:?}",
        run.log
    );
    assert!(
        run.log.iter().any(|l| l.contains("builds against its own proposal")),
        "the granting part was never handed its own proposal, so it could never \
         demonstrate it: {:?}",
        run.log
    );
    let latest = registry.current().expect("the contract reads back");
    assert!(
        latest.number > v1.number && latest.body.contains("total_pages"),
        "the contract never moved: v{} {:?}",
        latest.number,
        latest.body
    );
    assert!(latest.canonical, "what the parts build against is always ratified");

    assert!(
        run.composition.blocked.is_empty(),
        "a decomposed run needs every part: {:?}",
        run.composition.blocked
    );
    assert_eq!(run.composition.winners().len(), 2);

    assert!(run.landable(), "nothing to land: {:?}", run.blocked);
    let changes = run.changes.as_ref().expect("landable means there is a tree");
    let paths: Vec<&str> =
        changes.as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert!(paths.contains(&"src/api.rs"), "the backend's work is in the join: {paths:?}");
    assert!(paths.contains(&"ui/app.ts"), "the frontend's work is in the join: {paths:?}");
    assert!(
        !paths.contains(&compose::REQUEST_PATH),
        "a question must not land in the pull request: {paths:?}"
    );

    // --- the parts learned, and not from each other's pool ---
    // The frontend failed twice before the contract moved, so it has something to say; the pool is
    // keyed by the goal that was ASKED. Asserted, since a pool that quietly does nothing is the
    // failure mode.
    let fe_goal = "Render the results with a pager, against the fixtures in .contract-mocks.";
    let learned = pool
        .recall(
            fe_goal,
            &comp_goal::memory::Reading {
                k: 5,
                budget: 1200,
                pools: vec![],
                tags: vec![],
                min_similarity: 0.0,
            },
        )
        .expect("the pool answered");
    assert!(
        !learned.is_empty(),
        "the frontend failed twice and wrote nothing — the decomposed path is not learning"
    );
    assert!(
        learned.iter().any(|l| l.ns == "errors" && l.text.contains("pager-renders")),
        "a lesson should name the check that failed: {:?}",
        learned.iter().map(|l| &l.text).collect::<Vec<_>>()
    );

    let report = run.report.as_ref().expect("landable means the gate ran");
    assert!(report.passed, "two green halves did not compose: {:?}", report.failures);
    println!(
        "\n  composition: PASSED at score {} on contract v{}",
        report.score, run.composition.contract_version
    );
}
