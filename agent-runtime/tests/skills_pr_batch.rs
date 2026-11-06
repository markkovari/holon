//! Verify graph-native skills, autonomous agent PR proposing with graph provenance,
//! and free-tier batch queue model completion ($0.00 spend).

use agent_runtime::agent::Cause;
use agent_runtime::model::{LocalModel, Msg};
use agent_runtime::runtime::{Config, Runtime};
use agent_runtime::spec::{AgentSpec, Capability, ModelSpec};
use agent_wire::Step;
use std::sync::Arc;
use std::sync::Mutex;

fn runtime(tag: &str) -> Arc<Runtime> {
    let dir =
        tempfile::Builder::new().prefix(&format!("ar-skills-{tag}-")).tempdir().unwrap().keep();
    Runtime::new(Config::new(dir)).unwrap()
}

fn agent_with_replies(name: &str, replies: &[&str]) -> AgentSpec {
    let mut s = AgentSpec::new(name, "a test agent");
    s.model = ModelSpec::Mock { replies: replies.iter().map(|r| r.to_string()).collect() };
    s
}

#[test]
fn test_skill_query_tool_resolves_graph_subskills_and_wit_interfaces() {
    let rt = runtime("skill-query");

    let mut a = agent_with_replies(
        "architect",
        &[
            r#"{"tool": "skill_query", "args": {"skill_id": "agent:deliver_feature"}}"#,
            "I reviewed the skill DAG and resolved its prerequisite WIT interfaces.",
        ],
    );
    a.capabilities.push(Capability::named("skill_query"));
    rt.create_agent(a).unwrap();

    let rec = rt
        .run_agent(
            "architect",
            "http",
            "how do we deliver this feature?",
            &Cause::default(),
            "",
            true,
        )
        .unwrap();

    let tool_step = rec
        .steps
        .iter()
        .find_map(|s| match s {
            Step::Tool { name, result, error, .. } if name == "skill_query" => {
                Some((result.clone(), *error))
            }
            _ => None,
        })
        .expect("skill_query tool step found");

    assert!(!tool_step.1);
    assert!(tool_step.0.contains("git:forge/repo@0.1.0"));
    assert!(tool_step.0.contains("code:generate_batch"));
    assert!(tool_step.0.contains("rust:compile_check"));
}

#[test]
fn test_agent_propose_pr_records_provenance_and_returns_opened_record() {
    let rt = runtime("propose-pr");

    let mut a = agent_with_replies(
        "coder",
        &[
            r#"{"tool": "propose_pr", "args": {"branch": "feat/skill-graph-pr", "title": "feat(core): add skill graph and forge pr", "body": "Resolves issue #42", "message": "feat(core): add skill graph and forge pr"}}"#,
            "PR opened successfully with provenance recorded.",
        ],
    );
    a.capabilities.push(Capability::named("propose_pr"));
    a.auto_approve.push("propose_pr".into());
    rt.create_agent(a).unwrap();

    let rec = rt
        .run_agent("coder", "http", "open a PR for this work", &Cause::default(), "", true)
        .unwrap();

    let tool_step = rec
        .steps
        .iter()
        .find_map(|s| match s {
            Step::Tool { name, result, error, .. } if name == "propose_pr" => {
                Some((result.clone(), *error))
            }
            _ => None,
        })
        .expect("propose_pr tool step found");

    assert!(!tool_step.1);
    assert!(tool_step.0.contains("https://github.com/markkovari/holon/pull/"));
    assert!(tool_step.0.contains("feat/skill-graph-pr"));

    // Verify PR provenance was recorded in knowledge store
    let rows = rt.kv().list("pr_provenance", "pr:").unwrap();
    assert!(!rows.is_empty());
    assert!(rows
        .iter()
        .any(|(k, entry)| k.starts_with("pr:") && entry.value.contains("feat/skill-graph-pr")));
}

#[test]
fn test_free_tier_batch_queue_model_spec_completion() {
    let spec = ModelSpec::BatchQueue {
        queue_url: "http://127.0.0.1:4222".into(),
        model: "gemini-2.5-flash-lite".into(),
    };
    let local = LocalModel::default();
    let mock_cursor = Mutex::new(0);
    let msgs = vec![Msg::user("write a rust function for adding two numbers")];

    let reply =
        agent_runtime::model::complete(&spec, &local, "System coder prompt", &msgs, &mock_cursor)
            .unwrap();
    assert!(reply.text.contains("gemini-2.5-flash-lite"));
    assert!(reply.text.contains("free-tier"));
    assert_eq!(spec.label(), "batch:gemini-2.5-flash-lite");
}
