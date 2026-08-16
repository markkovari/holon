//! A decomposed goal, end to end: two parts, a contract, a negotiation, and one
//! joined tree judged by the same gate that judged the halves (ADR-0086).
//!
//! Eleven components across three apps, a real SurrealDB, a real native checks
//! runner, and a scripted model — so what is asserted is the ORCHESTRATION and not
//! a language model's mood.
//!
//! The test is arranged so that **the frontend cannot go green unless the
//! negotiation actually happened**. Its script has exactly two rules: one that
//! writes a request when it is told to render results, and one that writes working
//! code when it sees `total_pages`. `total_pages` is not in the first contract. So
//! a passing frontend means the request was read out of its candidate, answered by
//! a second model call, written to the registry, ratified by the backend's own
//! passing gate, and laid back into the next generation's prompt as `CONTRACT.md`.
//! Nothing else in this test could have produced it.
//!
//! Skipped, loudly, when Docker cannot start the database.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use comp_reconciler::compose;
use comp_reconciler::contract::{Answerer, Registry};
use comp_reconciler::fleet::{bin_path, free_port, repo_root, Fleet};
use comp_reconciler::generation::{compose_search, Bounds, Part};
use serde_json::{json, Value};

mod harness;
use harness::{Surreal, SURREAL_IMAGE, SURREAL_PASSWORD};

/// The commit the base tree is cached under.
///
/// Not empty: the runner caches a posted tree by commit and refuses to guess when
/// it has neither ("this runner has not seen that commit"). An empty one made
/// every check fail before a single candidate was judged, which reads in the log
/// as `need the tree for (no commit given)`.
const BASE: &str = "compose-base-1";

/// The interface the human wrote. Note what is NOT in it: `total_pages`.
const CONTRACT_V1: &str = r#"{"routes":[{"method":"GET","path":"/api/search","example":{"hits":[],"has_more":false}}]}"#;

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let mut out = Vec::new();
    for (id, file) in [
        // the driver stack
        ("probe", "driver_probe.wasm"),
        ("driver", "agent_driver.wasm"),
        ("agent", "agent_writer.wasm"),
        ("llm", "mock_provider.wasm"),
        ("gate", "checks_runner.wasm"),
        // the registry
        ("cprobe", "contract_probe.wasm"),
        ("registry", "contract_registry.wasm"),
        ("cgraph", "knowledge_graph.wasm"),
        // the model that answers a request
        ("lprobe", "llm_probe.wasm"),
        ("allm", "mock_provider.wasm"),
        // the pool the parts read from and write to
        ("mprobe", "memory_probe.wasm"),
        ("memory", "knowledge_memory.wasm"),
        ("graph", "knowledge_graph.wasm"),
        ("search", "search_index.wasm"),
        ("mllm", "mock_provider.wasm"),
    ] {
        let p = dir.join(file);
        assert!(p.exists(), "missing {} — run `just build`", p.display());
        out.push(format!("{id}={}", p.display()));
    }
    out
}

/// The gate, as a real process. The same runner the branches are judged by is the
/// one the composition is judged by — a join judged by different machinery is a
/// join whose failures are arguments about the harness.
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

/// A part's plan. `writable` is disjoint per part on purpose — that is most of
/// what makes them separate parts, and `compose::merge` refuses an overlap.
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
    let pool = comp_reconciler::memory::Memory {
        url: format!("http://127.0.0.1:{port}"),
        host: "memory.acme.test".into(),
        timeout: Duration::from_secs(30),
    };
    let answerer = Answerer {
        url: format!("http://127.0.0.1:{port}"),
        host: "composeanswer.acme.test".into(),
        timeout: Duration::from_secs(60),
    };

    // --- the human's contract -------------------------------------------------
    //
    // Retried until it lands: this is the first call that touches SurrealDB
    // through two components, so it is also the readiness check — what is retried
    // is what is measured.
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

    // --- the two parts --------------------------------------------------------
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
            // Neither the id NOR THE COMMAND may contain the trigger string. A
            // repair prompt carries the failing check's command, so
            // `grep -q total_pages` quoted the answer back at the model and the
            // frontend passed in round one without asking anybody anything. The
            // scripted model matches on everything the caller said — the same leak
            // a real model would read, which is why this is a comment and not a
            // quiet edit.
            json!([check("pager-renders", &["grep", "-q", "pager", "ui/app.ts"])]),
        ),
    ];

    // --- the run, through the SAME code `comp goal run` uses -------------------
    //
    // Not a re-spelling of it. `compose::run_parts` is the orchestration, and the
    // binary is a caller that prints and lands — so what this asserts is what a
    // real run does, minus argument parsing and a forge.
    let run = compose::run_parts(
        &compose::Wiring {
            driver_url: &format!("http://127.0.0.1:{port}/run"),
            driver_host: "composedrive.acme.test",
            checks_url: &format!("http://127.0.0.1:{}/check", checks.port),
            registry: &registry,
            answerer: Some(&answerer),
            // The parts read, write and attribute against a real pool here too, so
            // the decomposed path is covered by the same claim the ordinary one is.
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
        // The goal's own checks, which belong to neither part: the frontend reads a
        // field the backend serves, and only the joined tree can show it.
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

    // --- the negotiation happened ---------------------------------------------
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

    // --- and both halves are green --------------------------------------------
    assert!(
        run.composition.blocked.is_empty(),
        "a decomposed run needs every part: {:?}",
        run.composition.blocked
    );
    assert_eq!(run.composition.winners().len(), 2);

    // --- one joined tree, judged by the same gate -----------------------------
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

    // --- the parts learned, and not from each other's pool ---------------------
    //
    // The frontend failed twice before the contract moved, so it has something to
    // say; the pool is keyed by the goal that was ASKED, and a part asks about its
    // own. Asserted rather than assumed: wiring a pool in and having it quietly do
    // nothing is the failure mode this whole session kept finding.
    let fe_goal = "Render the results with a pager, against the fixtures in .contract-mocks.";
    let learned = pool
        .recall(fe_goal, &comp_reconciler::memory::Reading { k: 5, budget: 1200, pools: vec![], tags: vec![] })
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
