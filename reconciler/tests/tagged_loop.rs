//! The loop closes around a TAG: a run passes only because a differently-worded
//! run left a lesson against an interface it shares.
//!
//! `learning.rs` proves the loop closes around text — same goal twice, the second
//! run reading what the first wrote. This proves the harder and more valuable
//! case, which is the one a capability library is for: two goals that have
//! **nothing in common except an interface**, where similarity has nothing to work
//! with and the structural key is the only thing connecting them (ADR-0090).
//!
//! It is arranged so that nothing else can produce the result. The scripted model
//! has three rules and only one of them writes a passing answer:
//!
//!   · a prompt containing `AVOID` — the marker a retrieved `errors` lesson is
//!     rendered under — writes `42`, which passes the gate.
//!   · the first goal's exact wording writes `41`, which fails.
//!   · anything else writes `0`, which fails.
//!
//! So the second goal CANNOT pass on its own. `AVOID` reaches its prompt only if a
//! lesson was retrieved, and the two goals share no wording, so a retrieval that
//! ignores tags has no way to find it. Three arms make that falsifiable rather
//! than asserted:
//!
//!   1. **tagged** — must pass.
//!   2. **text only** — must FAIL. If this passes, similarity already connected
//!      the two goals, tags bought nothing, and the design is wrong.
//!   3. **cold**, reading nothing — must fail, or arm 1 proves nothing at all.
//!
//! No AI calls. A real SurrealDB, a real fleet of eight components, the real
//! native checks runner, and a deterministic provider — so a green run means the
//! machinery works rather than that a model was in a good mood.
//!
//! Skipped, loudly, when Docker cannot start the database.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use comp_reconciler::fleet::{bin_path, free_port, repo_root, Fleet};
use comp_reconciler::generation::{default_strategies, search_with, Bounds, SearchStop};
use comp_reconciler::memory::{self, run_id, Memory, Reading};
use serde_json::{json, Value};

mod harness;
use harness::{Surreal, SURREAL_IMAGE, SURREAL_PASSWORD};

const BASE: &str = "tagged-loop-base-1";

/// The two goals, side by side, because their DISSIMILARITY is the experiment.
///
/// One is the wording the script knows and fails on; the other shares not a single
/// content word with it. If these ever drift towards each other the control arm
/// starts passing and the test quietly stops meaning anything, so they are stated
/// together where that is visible.
const FIRST_GOAL: &str = "make the answer 42";
const SECOND_GOAL: &str =
    "produce a monthly payroll remittance file for the finance team, totalled per employee";

/// What both pieces of work touch. Fictional in this test only in the sense that
/// no component here really imports it — the point is that it is an identifier
/// shared by two goals, which is exactly what `plug::tags_for` produces from a
/// real part's artifact.
const SHARED_TAG: &str = "csv:codec/codec@0.1.0";

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    [
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
    ]
    .iter()
    .map(|(id, file)| {
        let p = dir.join(file);
        assert!(p.exists(), "missing {} — run `just build`", p.display());
        format!("{id}={}", p.display())
    })
    .collect()
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
        let out = std::env::temp_dir().join(format!("comp-tagloop-{name}-{surreal_port}"));
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
            // over and the model never needs the lesson.
            "command": ["grep", "-q", "42", "src/lib.rs"],
        }],
        "base_commit": BASE,
        "base_tree": base_tree(),
        "max_attempts": 1,
        "seed": 1,
    })
}

#[test]
fn a_lesson_reaches_a_goal_that_shares_only_an_interface() {
    let Some(db) = Surreal::start() else {
        eprintln!(
            "SKIPPED: could not start {SURREAL_IMAGE} — this test needs a real database and \
             Docker to run it in. Nothing about tagged retrieval closing the loop was \
             verified by this run."
        );
        return;
    };
    let checks = Checks::start();

    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let spec_files = specs(db.port, checks.port);
    let spec_refs: Vec<&str> = spec_files.iter().map(|p| p.to_str().unwrap()).collect();
    let fleet = Fleet::start_with_secrets(
        "tagloop",
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
    fleet.until("asking the pool about a goal nobody has run", Duration::from_secs(180), || {
        pool.already_done("nothing has ever asked this", 0.9).map(|_| ())
    });

    let bounds = Bounds { branches: 1, max_rounds: 1, max_tokens: 0, patience: 0 };

    // --- the first goal fails, and what it failed on becomes a TAGGED lesson ----
    let first = search_with(
        &driver_url,
        "learndrive.acme.test",
        &plan(FIRST_GOAL),
        &default_strategies(1),
        bounds,
        21_000,
        Duration::from_secs(120),
    );
    let attempt = first.best.as_ref().expect("a branch ran");
    assert!(!attempt.accepted, "the script writes 41 for this prompt: {:?}", attempt.files);

    let text = memory::failure_text(&attempt.failures, attempt.score)
        .expect("a failed branch has something to teach");
    let handle = pool
        .observe_failure_tagged(
            FIRST_GOAL,
            "branch-0",
            &run_id(21_000, 0, "branch-0"),
            &text,
            &[SHARED_TAG.to_string()],
        )
        .expect("the pool took the tagged lesson");
    assert!(handle.starts_with("errors:"), "negative knowledge goes to errors: {handle}");

    // --- arm 2 first: the second goal, reading by TEXT only ---------------------
    //
    // Run before the tagged arm deliberately. If it passed, the tagged arm would
    // prove nothing, and finding that out BEFORE the positive result is what stops
    // a green test from being read as a green claim.
    //
    // `min_similarity` is set because the mock provider's embeddings are a
    // deterministic function of the text rather than a language model's: without a
    // floor, two unrelated goals can land arbitrarily close and the arm would be
    // measuring the fixture instead of the design.
    let text_only = Reading {
        k: 3,
        budget: 1200,
        pools: vec![],
        tags: vec![],
        min_similarity: 0.55,
    };
    let by_text = pool.recall(SECOND_GOAL, &text_only).expect("the pool answered");
    let text_render = memory::render(&by_text);
    assert!(
        !text_render.contains("AVOID"),
        "text similarity already reaches a lesson written under {FIRST_GOAL:?} from \
         {SECOND_GOAL:?}, so tags are not what would connect them and ADR-0090's \
         premise is wrong. Retrieved: {text_render}"
    );

    let mut cold_arm = default_strategies(1);
    cold_arm[0].knowledge = text_render.clone();
    let text_run = search_with(
        &driver_url,
        "learndrive.acme.test",
        &plan(SECOND_GOAL),
        &cold_arm,
        bounds,
        22_000,
        Duration::from_secs(120),
    );
    assert!(
        !text_run.best.as_ref().expect("a branch ran").accepted,
        "the text-only branch passed, so the second goal does not actually need the \
         lesson and this test measures nothing"
    );

    // --- arm 1: the same goal, carrying the interface it imports ----------------
    let tagged = Reading {
        k: 3,
        budget: 1200,
        pools: vec![],
        tags: vec![SHARED_TAG.to_string()],
        min_similarity: 0.55,
    };
    let by_tag = pool.recall(SECOND_GOAL, &tagged).expect("the pool answered");
    let tag_render = memory::render(&by_tag);
    assert!(
        tag_render.contains("AVOID"),
        "the lesson was written against {SHARED_TAG} and a goal importing it did not \
         get it back: {tag_render}"
    );

    let mut tagged_arm = default_strategies(1);
    tagged_arm[0].knowledge = tag_render.clone();
    let tagged_run = search_with(
        &driver_url,
        "learndrive.acme.test",
        &plan(SECOND_GOAL),
        &tagged_arm,
        bounds,
        23_000,
        Duration::from_secs(120),
    );
    let learned = tagged_run.best.as_ref().expect("a branch ran");
    assert!(
        learned.accepted,
        "the branch that read a lesson found BY TAG still failed — the loop does not \
         close around the structural key: {:?}",
        learned.files
    );
    assert!(matches!(tagged_run.stopped, SearchStop::Accepted), "{:?}", tagged_run.stopped);

    // --- arm 3: reading nothing at all ------------------------------------------
    //
    // The floor. Without it, "the tagged branch passed" is compatible with the
    // second goal being winnable on its own.
    let cold = search_with(
        &driver_url,
        "learndrive.acme.test",
        &plan(SECOND_GOAL),
        &default_strategies(1),
        bounds,
        24_000,
        Duration::from_secs(120),
    );
    assert!(
        !cold.best.as_ref().expect("a branch ran").accepted,
        "the cold branch passed too, so nothing here is evidence of anything"
    );

    // --- and what was read is attributed, as any other reading would be ---------
    let keys: Vec<String> = by_tag.iter().map(|l| l.key.clone()).collect();
    pool.attribute(&keys, &run_id(23_000, 0, "branch-0"), true).expect("attribution landed");

    println!(
        "\n  a lesson written under {FIRST_GOAL:?} reached {SECOND_GOAL:?} — which shares \
         one interface with it and no wording — and only the branch that read it passed"
    );
}
