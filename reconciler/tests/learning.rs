//! The loop learns: a run writes what it failed on, and a later run can only
//! succeed by having read it.
//!
//! This is the claim the knowledge layer exists to make, and it is arranged so
//! that nothing else can produce the result. The scripted model has exactly two
//! rules that matter: it writes the WRONG answer for the goal as written, and the
//! right one when it sees `AVOID` — the tag a retrieved `errors` lesson is
//! rendered under. `AVOID` cannot appear in the second prompt unless the first
//! run's failure became a lesson, the lesson was retrieved, and the retrieval was
//! rendered into the branch's goal.
//!
//! No AI calls: eight components, a real SurrealDB, the real native checks runner,
//! and a deterministic provider — so a green run means the machinery works rather
//! than that a model was in a good mood. `reconciler/tests/memory.rs` covers the
//! pool itself; this covers the LOOP closing around it.
//!
//! Skipped, loudly, when Docker cannot start the database.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use comp_reconciler::fleet::{bin_path, free_port, repo_root, Fleet};
use comp_reconciler::generation::{default_strategies, search_with, Bounds};
use comp_reconciler::memory::{self, run_id, Memory, Reading};
use serde_json::{json, Value};

mod harness;
use harness::{Surreal, SURREAL_IMAGE, SURREAL_PASSWORD};

const BASE: &str = "learning-base-1";

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let mut out = Vec::new();
    for (id, file) in [
        ("probe", "driver_probe.wasm"),
        ("driver", "agent_driver.wasm"),
        ("agent", "agent_writer.wasm"),
        ("llm", "mock_provider.wasm"),
        ("gate", "checks_runner.wasm"),
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
            .args(["--allow", "grep", "--timeout", "30"])
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
        ("learning-driver.yaml", vec![("CHECKS_PORT", checks_port.to_string())]),
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
    json!([{ "path": "src/lib.rs", "content": "pub fn answer() -> u32 { 0 }\n" }])
}

fn plan(text: &str) -> Value {
    json!({
        "text": text,
        "writable": ["src/lib.rs"],
        "context": base_tree(),
        "previous": [],
        "checks": [{
            "id": "answer-is-42",
            "required": true,
            "weight": 1,
            // The command must not NAME the answer, or a repair prompt hands it
            // over and the model never needs the lesson (measured, the hard way,
            // in the contract e2e).
            "command": ["grep", "-q", "42", "src/lib.rs"],
        }],
        "base_commit": BASE,
        "base_tree": base_tree(),
        "max_attempts": 1,
        "seed": 1,
    })
}

#[test]
fn a_run_writes_what_it_failed_on_and_a_later_run_reads_it() {
    let Some(db) = Surreal::start() else {
        eprintln!(
            "SKIPPED: could not start {SURREAL_IMAGE} — this test needs a real database and \
             Docker to run it in. Nothing about the learning loop was verified by this run."
        );
        return;
    };
    let checks = Checks::start();

    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let spec_files = specs(db.port, checks.port);
    let spec_refs: Vec<&str> = spec_files.iter().map(|p| p.to_str().unwrap()).collect();
    let fleet = Fleet::start_with_secrets(
        "learning",
        &spec_refs,
        &artifacts(),
        &[format!("vault://acme/surreal={SURREAL_PASSWORD}")],
    );
    let port = fleet.ingress_port;
    let driver_url = format!("http://127.0.0.1:{port}/run");

    let pool = Memory {
        url: format!("http://127.0.0.1:{port}"),
        host: "memory.acme.test".into(),
        timeout: Duration::from_secs(30),
    };
    // The first real call is the readiness check: it can only answer by reaching
    // SurrealDB through two components (the mistake `Fleet::until` exists for).
    fleet.until("asking the pool about a goal nobody has run", Duration::from_secs(180), || {
        pool.already_done("nothing has ever asked this", 0.9).map(|_| ())
    });

    let goal = "make the answer 42";
    let bounds = Bounds { branches: 1, max_rounds: 1, max_tokens: 0, patience: 0 };

    // --- run one: it fails, and writes down why -------------------------------
    let first = search_with(
        &driver_url,
        "learndrive.acme.test",
        &plan(goal),
        &default_strategies(1),
        bounds,
        11_000,
        Duration::from_secs(120),
    );
    let attempt = first.best.as_ref().expect("a branch ran");
    assert!(!attempt.accepted, "the script writes 41 for this prompt: {:?}", attempt.files);

    let text = memory::failure_text(&attempt.failures, attempt.score)
        .expect("a failed branch has something to teach");
    assert!(text.contains("answer-is-42"), "the lesson names the check: {text}");
    let handle = pool
        .observe_failure(goal, "branch-0", &run_id(11_000, 0, "branch-0"), &text)
        .expect("the pool took the lesson");
    assert!(handle.starts_with("errors:"), "negative knowledge goes to errors: {handle}");

    // --- the lesson comes back, and is rendered as advice ----------------------
    let lessons = pool
        .recall(
            goal,
            &Reading { k: 3, budget: 1200, pools: vec![], tags: vec![], min_similarity: 0.0 },
        )
        .expect("the pool answered");
    assert!(!lessons.is_empty(), "a lesson written is a lesson findable");
    let rendered = memory::render(&lessons);
    assert!(rendered.contains("[AVOID]"), "an errors lesson is marked as one: {rendered}");
    assert!(rendered.contains("answer-is-42"), "{rendered}");

    // --- run two: the SAME goal, one branch reading -----------------------------
    //
    // The script cannot produce 42 for this goal on its own — it writes 41 — so a
    // pass here means the lesson reached the prompt. Nothing else in the fixture
    // can produce that.
    let mut strategies = default_strategies(1);
    strategies[0].knowledge = rendered.clone();
    let second = search_with(
        &driver_url,
        "learndrive.acme.test",
        &plan(goal),
        &strategies,
        bounds,
        12_000,
        Duration::from_secs(120),
    );
    let learned = second.best.as_ref().expect("a branch ran");
    assert!(
        learned.accepted,
        "the branch that read the lesson still failed — the loop did not learn: {:?}",
        learned.files
    );
    assert!(
        matches!(second.stopped, comp_reconciler::generation::SearchStop::Accepted),
        "{:?}",
        second.stopped
    );

    // --- and a branch that reads nothing still fails ---------------------------
    //
    // The control arm, and the only thing that makes the assertion above mean
    // anything: same goal, same script, same everything except the reading.
    let cold = search_with(
        &driver_url,
        "learndrive.acme.test",
        &plan(goal),
        &default_strategies(1),
        bounds,
        13_000,
        Duration::from_secs(120),
    );
    assert!(
        !cold.best.as_ref().expect("a branch ran").accepted,
        "the cold branch passed too, so the second run proves nothing about reading"
    );

    // --- what was read is attributed to what happened --------------------------
    let keys: Vec<String> = lessons.iter().map(|l| l.key.clone()).collect();
    pool.attribute(&keys, &run_id(12_000, 0, "branch-0"), true).expect("attribution landed");
    pool.attribute(&keys, &run_id(13_000, 0, "branch-0"), false).expect("attribution landed");
    // Still retrievable afterwards: attribution moves standing, it does not delete.
    assert!(
        !pool
            .recall(
                goal,
                &Reading { k: 3, budget: 1200, pools: vec![], tags: vec![], min_similarity: 0.0 }
            )
            .unwrap()
            .is_empty(),
        "sinking is not deletion"
    );

    // --- and what the gate proved can be promoted ------------------------------
    //
    // The trusted pool has one writer, and it is not the agent-facing interface:
    // `promote` goes through `knowledge:memory/promotion`, which an agent's world
    // does not contain (ADR-0084). The distiller's own model call is not exercised
    // here — its prompt and its parser are unit-tested, and a scripted door would
    // only be testing the script.
    let lesson = "the gate greps the file rather than running it, so the answer has to be \
                  literal in the source";
    let promoted = pool
        .promote(goal, "branch-0", &run_id(12_000, 0, "branch-0"), lesson, 1000)
        .expect("a passing score may promote");
    assert!(promoted.starts_with("patterns:"), "promotion writes the trusted pool: {promoted}");

    // A score that did not pass may not, and the refusal comes from the component
    // rather than from politeness here.
    let refused = pool
        .promote(goal, "branch-0", "run-x", lesson, 0)
        .expect_err("a gate that did not pass must not promote");
    assert!(refused.contains("refused"), "{refused}");

    // And the promoted lesson comes back marked as what it is.
    let after = pool
        .recall(
            goal,
            &Reading {
                k: 5,
                budget: 1200,
                pools: vec!["patterns".into()],
                tags: vec![],
                min_similarity: 0.0,
            },
        )
        .expect("the pool answered");
    let rendered_after = memory::render(&after);
    assert!(rendered_after.contains("[PROVEN]"), "a pattern is not a guess: {rendered_after}");
    assert!(rendered_after.contains("greps the file"), "{rendered_after}");

    println!(
        "\n  learned: run one failed and wrote {handle}; run two read it and passed; \
         a branch that read nothing failed; the winner's lesson promoted to {promoted}"
    );
}
