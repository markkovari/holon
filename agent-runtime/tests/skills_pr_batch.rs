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
fn test_skill_activate_wit_contract_first_scaffold() {
    let rt = runtime("activate-wit");

    let mut a = agent_with_replies(
        "scaffolder",
        &[
            r#"{"tool": "skill_activate", "args": {"skill_id": "wit:contract_first_scaffold"}}"#,
            "I have loaded the WIT contract-first scaffolding plan.",
        ],
    );
    a.capabilities.push(Capability::named("skill_activate"));
    rt.create_agent(a).unwrap();

    let rec = rt
        .run_agent("scaffolder", "http", "prepare scaffolding", &Cause::default(), "", true)
        .unwrap();

    let tool_step = rec
        .steps
        .iter()
        .find_map(|s| match s {
            Step::Tool { name, result, error, .. } if name == "skill_activate" => {
                Some((result.clone(), *error))
            }
            _ => None,
        })
        .expect("skill_activate tool step found");

    assert!(!tool_step.1);
    assert!(tool_step.0.contains("wit:contract_first_scaffold"));
    assert!(tool_step.0.contains("wit:draft_interface"));
    assert!(tool_step.0.contains("bindings:generate_xtask"));
    assert!(tool_step.0.contains("wit:reflect@0.1.0"));
}

#[test]
fn test_skill_activate_bug_repro_and_repair() {
    let rt = runtime("activate-bug");

    let mut a = agent_with_replies(
        "debugger",
        &[
            r#"{"tool": "skill_activate", "args": {"skill_id": "bug:autonomous_repro_and_repair"}}"#,
            "I have activated the autonomous bug reproduction and gate repair plan.",
        ],
    );
    a.capabilities.push(Capability::named("skill_activate"));
    rt.create_agent(a).unwrap();

    let rec =
        rt.run_agent("debugger", "http", "fix issue #102", &Cause::default(), "", true).unwrap();

    let tool_step = rec
        .steps
        .iter()
        .find_map(|s| match s {
            Step::Tool { name, result, error, .. } if name == "skill_activate" => {
                Some((result.clone(), *error))
            }
            _ => None,
        })
        .expect("skill_activate tool step found");

    assert!(!tool_step.1);
    assert!(tool_step.0.contains("bug:autonomous_repro_and_repair"));
    assert!(tool_step.0.contains("test:author_failing_gate"));
    assert!(tool_step.0.contains("gate:prove_initial_failure"));
    assert!(tool_step.0.contains("graph:fitness/evaluator"));
}

#[test]
fn test_agent_with_configured_skills_loads_dags_into_system_prompt() {
    let rt = runtime("spec-skills");

    let mut a = agent_with_replies("lead_eng", &["I am ready with both skills loaded."]);
    a.skills = vec![
        "wit:contract_first_scaffold".to_string(),
        "bug:autonomous_repro_and_repair".to_string(),
    ];
    let prompt = agent_runtime::agent::system_prompt(rt.as_ref(), &a);
    assert!(prompt.contains("Active Graph-Native Skills:"));
    assert!(prompt.contains("[wit:contract_first_scaffold]"));
    assert!(prompt.contains("wit:draft_interface"));
    assert!(prompt.contains("bindings:generate_xtask"));
    assert!(prompt.contains("[bug:autonomous_repro_and_repair]"));
    assert!(prompt.contains("test:author_failing_gate"));
    assert!(prompt.contains("gate:verify_green"));
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

#[test]
fn test_skill_curator_agent_audits_registry_and_proposes_refactor_pr() {
    let rt = runtime("curator");

    let mut a = agent_with_replies(
        "curator-agent",
        &[
            r#"{"tool": "skill_audit", "args": {}}"#,
            r#"{"tool": "propose_pr", "args": {"branch": "refactor/dedup-skills", "title": "refactor(skills): deduplicate registry and alias legacy workflows", "body": "Periodic curator audit detected near-duplicate workflows; consolidated definitions."}}"#,
            "Curator audit completed and PR opened.",
        ],
    );
    a.capabilities.push(Capability::named("skill_audit"));
    a.capabilities.push(Capability::named("propose_pr"));
    a.auto_approve.push("skill_audit".into());
    a.auto_approve.push("propose_pr".into());
    a.skills.push("skill:curate_registry".into());
    rt.create_agent(a).unwrap();

    let rec = rt
        .run_agent(
            "curator-agent",
            "cron",
            "run periodic hygiene review",
            &Cause::default(),
            "",
            true,
        )
        .unwrap();

    let audit_step = rec
        .steps
        .iter()
        .find_map(|s| match s {
            Step::Tool { name, result, error, .. } if name == "skill_audit" => {
                Some((result.clone(), *error))
            }
            _ => None,
        })
        .expect("skill_audit tool step found");

    assert!(!audit_step.1);
    assert!(audit_step.0.contains("total_skills"));

    let pr_step = rec
        .steps
        .iter()
        .find_map(|s| match s {
            Step::Tool { name, result, error, .. } if name == "propose_pr" => {
                Some((result.clone(), *error))
            }
            _ => None,
        })
        .expect("propose_pr tool step found");

    assert!(!pr_step.1);
    assert!(pr_step.0.contains("refactor/dedup-skills"));
}
