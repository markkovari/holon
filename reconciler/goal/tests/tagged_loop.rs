//! Two goals sharing only an interface: the second passes only via a tagged lesson (ADR-0090).
//! Arms: tagged must pass; text-only and cold must fail, or tags bought nothing.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use comp_fleet::fleet::{bin_path, free_port, repo_root, Fleet};
use comp_goal::generation::{default_strategies, search_with, Bounds, SearchStop};
use comp_goal::memory::{self, run_id, Memory, Reading};
use serde_json::{json, Value};

// Shared harness, included rather than copied (docs/adr/0115).
#[path = "../../tests/harness/mod.rs"]
mod harness;
use harness::{Surreal, SURREAL_IMAGE, SURREAL_PASSWORD};

const BASE: &str = "tagged-loop-base-1";

/// Their dissimilarity is the experiment: if they drift together the control arm passes.
const FIRST_GOAL: &str = "make the answer 42";
const SECOND_GOAL: &str =
    "produce a monthly payroll remittance file for the finance team, totalled per employee";

/// As `plug::tags_for` produces from a real part's artifact.
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
        assert!(p.exists(), "missing {} — run `cargo xtask build --force`", p.display());
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
                        // The command must not NAME the answer, or a repair prompt hands it over.
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

    // Text-only first, so a passing control shows before the positive result. The floor stops mock
    // embeddings landing unrelated goals arbitrarily close.
    let text_only =
        Reading { k: 3, budget: 1200, pools: vec![], tags: vec![], min_similarity: 0.55 };
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

    // Without this arm, a tagged pass is compatible with the goal being winnable alone.
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

    let keys: Vec<String> = by_tag.iter().map(|l| l.key.clone()).collect();
    pool.attribute(&keys, &run_id(23_000, 0, "branch-0"), true).expect("attribution landed");

    println!(
        "\n  a lesson written under {FIRST_GOAL:?} reached {SECOND_GOAL:?} — which shares \
         one interface with it and no wording — and only the branch that read it passed"
    );
}
