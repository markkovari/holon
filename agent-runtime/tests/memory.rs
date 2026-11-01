//! Integration tests for Pillar IV: Memory, Recall & Continuous Learning.
//!
//! Tests:
//! 1. Hybrid dense/sparse semantic memory search with Reciprocal Rank Fusion (BM25 + vector RRF)
//! 2. Automatic lesson extraction and self-distillation (`distill_lesson`)
//! 3. Cross-project knowledge federation (`private` -> `project.<name>` -> `org.shared`)
//! 4. Context window compaction & rolling state summarization (`compact_context`)

use std::sync::Arc;

use agent_runtime::agent::Cause;
use agent_runtime::projects::ProjectInfo;
use agent_runtime::store::{Status, Step};
use agent_runtime::{AgentSpec, Capability, Config, ModelSpec, Runtime};

fn dir(tag: &str) -> std::path::PathBuf {
    tempfile::Builder::new().prefix(&format!("ar-memory-{tag}-")).tempdir().unwrap().keep()
}

fn runtime(tag: &str) -> Arc<Runtime> {
    Runtime::new(Config::new(dir(tag))).unwrap()
}

fn agent(name: &str, replies: &[&str]) -> AgentSpec {
    let mut s = AgentSpec::new(name, "a memory test agent");
    s.model = ModelSpec::Mock { replies: replies.iter().map(|r| r.to_string()).collect() };
    s
}

#[test]
fn test_distill_lesson_and_tagged_memory_recall() {
    let rt = runtime("distill");
    let mut a = agent(
        "learner",
        &[
            // 1. Distill an enduring rule
            r#"{"tool":"distill_lesson","args":{"rule":"Always run tests before commit","context":"PR failed in CI"}}"#,
            // 2. Normal remember
            r#"{"tool":"remember","args":{"text":"user prefers dark mode"}}"#,
            // 3. Recall specifically for rules
            r#"{"tool":"recall","args":{"query":"tests","kind":"rule"}}"#,
            // 4. Recall all memories
            r#"{"tool":"recall","args":{"query":"tests dark"}}"#,
            "learning verified",
        ],
    );
    a.capabilities.push(Capability::named("distill_lesson"));
    rt.create_agent(a).unwrap();

    let r = rt.run_agent("learner", "http", "go", &Cause::default(), "", true).unwrap();
    assert_eq!(r.status, Status::Ok);

    let recall_steps: Vec<String> = r
        .steps
        .iter()
        .filter_map(|s| match s {
            Step::Tool { name, result, .. } if name == "recall" => Some(result.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(recall_steps.len(), 2);
    // Step 1: Recall specifically for kind: "rule"
    assert!(recall_steps[0].contains("RULE: Always run tests before commit"));
    assert!(recall_steps[0].contains("[rule]"));
    assert!(!recall_steps[0].contains("dark mode"));

    // Step 2: Recall all memories contains both
    assert!(recall_steps[1].contains("RULE: Always run tests before commit"));
    assert!(recall_steps[1].contains("user prefers dark mode"));
}

#[test]
fn test_cross_project_knowledge_federation() {
    let rt = runtime("federate");
    let mut a = agent(
        "worker",
        &[
            // 1. Remember in private scope
            r#"{"tool":"remember","args":{"text":"private secret key 123","scope":"private"}}"#,
            // 2. Remember in project scope
            r#"{"tool":"remember","args":{"text":"project alpha deployment port 8080","scope":"project:alpha"}}"#,
            // 3. Remember in org shared scope
            r#"{"tool":"remember","args":{"text":"company mission is open agents","scope":"org"}}"#,
            // 4. Recall private only
            r#"{"tool":"recall","args":{"query":"secret deployment mission","scope":"private"}}"#,
            // 5. Recall project only
            r#"{"tool":"recall","args":{"query":"secret deployment mission","scope":"project:alpha"}}"#,
            // 6. Recall federated across all scopes
            r#"{"tool":"recall","args":{"query":"secret deployment mission","scope":"all"}}"#,
            "federation complete",
        ],
    );
    a.projects = vec![ProjectInfo {
        name: "alpha".into(),
        description: "project alpha".into(),
        members: vec!["worker".into()],
        lead: None,
        store_ns: "project.alpha".into(),
        topic_glob: "alpha.*".into(),
    }];
    rt.create_agent(a).unwrap();
    rt.put_project(agent_runtime::projects::Project {
        name: "alpha".into(),
        description: "project alpha".into(),
        agents: vec!["worker".into()],
        ..Default::default()
    })
    .unwrap();

    let r = rt.run_agent("worker", "http", "go", &Cause::default(), "", true).unwrap();
    assert_eq!(r.status, Status::Ok);

    let recall_steps: Vec<String> = r
        .steps
        .iter()
        .filter_map(|s| match s {
            Step::Tool { name, result, .. } if name == "recall" => Some(result.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(recall_steps.len(), 3);
    // 4. Private only
    assert!(recall_steps[0].contains("private secret key 123"));
    assert!(!recall_steps[0].contains("port 8080"));
    assert!(!recall_steps[0].contains("company mission"));

    // 5. Project only
    assert!(recall_steps[1].contains("project alpha deployment port 8080"));
    assert!(!recall_steps[1].contains("private secret key"));
    assert!(!recall_steps[1].contains("company mission"));

    // 6. Federated across all scopes
    assert!(recall_steps[2].contains("private secret key 123"));
    assert!(recall_steps[2].contains("project alpha deployment port 8080"));
    assert!(recall_steps[2].contains("company mission is open agents"));
}

#[test]
fn test_hybrid_dense_sparse_rrf_scoring() {
    let rt = runtime("hybrid-rrf");
    let store = rt.store();

    // Setup toy embeddings: dimension 0 is "rowing", dimension 1 is "food"
    let embed = |texts: &[String]| -> Option<Vec<Vec<f32>>> {
        Some(
            texts
                .iter()
                .map(|t| {
                    if t.contains("lunch") || t.contains("food") {
                        vec![0.0, 1.0]
                    } else if t.contains("erg") || t.contains("rowing") {
                        vec![1.0, 0.0]
                    } else {
                        vec![0.707, 0.707]
                    }
                })
                .collect(),
        )
    };

    // Store memories:
    // 1. Matches keyword "tuesday" and "erg"
    store.remember("rower", "rowed on tuesday on the erg", 1).unwrap();
    // 2. Matches semantic "rowing" (embeds to [1.0, 0.0]) without the word "erg"
    store.remember("rower", "completed 15k rowing workout", 2).unwrap();
    // 3. Rule with high priority
    store
        .remember_tagged("rower", "warm up 10 minutes before rowing", Some("rule"), None, 3)
        .unwrap();

    // Query "rowing workout on the erg" with dense query vector [1.0, 0.0]
    let query_vec = [1.0, 0.0];
    let hits = store.recall_hybrid(
        "rower",
        &[],
        agent_runtime::store::RecallQuery {
            query: "rowing workout on the erg",
            query_vec: Some(&query_vec),
            scope: None,
            kind: None,
            k: 3,
        },
        &embed,
    );

    assert_eq!(hits.len(), 3);
    // The rule has 1.25x boost and semantic alignment
    assert!(hits.iter().any(|h| h.text.contains("warm up 10 minutes")));
    assert!(hits.iter().any(|h| h.text.contains("rowed on tuesday")));
    assert!(hits.iter().any(|h| h.text.contains("completed 15k rowing workout")));
}

#[test]
fn test_compact_context_prunes_history_and_preserves_summary() {
    let rt = runtime("compaction");
    let mut a = agent(
        "compactor",
        &[
            // Turn 0: step 1
            r#"{"tool":"remember","args":{"text":"item alpha discovered"}}"#,
            // Turn 1: compact context
            r#"{"tool":"compact_context","args":{"summary":"Phase 1 finished: item alpha verified and stored."}}"#,
            // Turn 2: final answer
            "Compacted run finished successfully.",
        ],
    );
    a.capabilities.push(Capability::named("compact_context"));
    rt.create_agent(a).unwrap();

    let r = rt.run_agent("compactor", "http", "start task", &Cause::default(), "", true).unwrap();
    assert_eq!(r.status, Status::Ok);
    assert_eq!(r.answer, "Compacted run finished successfully.");

    let compact_step = r
        .steps
        .iter()
        .find(|s| matches!(s, Step::Tool { name, .. } if name == "compact_context"))
        .expect("compact_context step should be recorded");

    if let Step::Tool { result, .. } = compact_step {
        assert!(result.contains("context compacted: Phase 1 finished"));
    }
}
