//! Tests for Agent Handoff and Context Routing Protocol (ADR-0129).

use std::sync::Arc;

use agent_runtime::agent::Cause;
use agent_runtime::runtime::{Config, Runtime};
use agent_runtime::spec::{AgentSpec, Capability, ModelSpec};
use agent_runtime::store::RunRecord;
use agent_wire::{HandoffResponse, HandoffStatus, Step};
use serde_json::json;

fn dir(tag: &str) -> std::path::PathBuf {
    tempfile::Builder::new().prefix(&format!("ar-handoff-{tag}-")).tempdir().unwrap().keep()
}

fn runtime(tag: &str) -> Arc<Runtime> {
    Runtime::new(Config::new(dir(tag))).unwrap()
}

fn agent(name: &str, replies: &[&str]) -> AgentSpec {
    let mut s = AgentSpec::new(name, "a test agent");
    s.model = ModelSpec::Mock { replies: replies.iter().map(|r| r.to_string()).collect() };
    s
}

fn runs(rt: &Runtime, name: &str) -> Vec<RunRecord> {
    let mut v = rt.store().runs(name, 100);
    v.reverse();
    v
}

#[test]
fn direct_agent_call_with_pass_by_reference_context_pointers() {
    let rt = runtime("pass-by-ref");

    // Seed the blackboard with stored analysis data
    rt.kv()
        .put("project.alpha", "raw_data", "metric1=42,metric2=99", "origin", 1000, None)
        .unwrap();

    // The target agent ("analyst") has permissions to read store, write workspace, and store results
    let mut analyst = agent(
        "analyst",
        &[
            r#"{"tool":"store_get","args":{"ns":"project.alpha","key":"raw_data"}}"#,
            r#"{"tool":"write_file","args":{"path":"analysis/report.md","content":"Metric1=42, Metric2=99"}}"#,
            r#"{"tool":"store_put","args":{"ns":"project.alpha","key":"summary","value":"metrics analyzed: all good"}}"#,
            "Analysis successfully completed and written to workspace and blackboard.",
        ],
    );
    for cap in ["store_get", "store_put", "write_file"] {
        analyst.capabilities.push(Capability::named(cap));
    }
    analyst.auto_approve.push("write_file".into());
    analyst.store.read.push("project.alpha".into());
    analyst.store.write.push("project.alpha".into());
    rt.create_agent(analyst).unwrap();

    // The caller agent ("manager") holds capability `agent:analyst`
    let call_args = json!({
        "tool": "agent:analyst",
        "args": {
            "task": "Process raw metrics into final summary",
            "intent": "execute",
            "context": {
                "store_refs": ["project.alpha/raw_data"]
            }
        }
    })
    .to_string();

    let mut manager = agent("manager", &[&call_args, "Manager saw analyst finish."]);
    manager.capabilities.push(Capability::named("agent:analyst"));
    rt.create_agent(manager).unwrap();

    // Run the manager
    let mgr_run =
        rt.run_agent("manager", "http", "start pipeline", &Cause::default(), "", true).unwrap();
    assert_eq!(mgr_run.steps.len(), 3); // Tool (call analyst), Model (manager final), etc.

    // Verify analyst ran and received the ambient prompt with context references
    let analyst_runs = runs(&rt, "analyst");
    assert_eq!(analyst_runs.len(), 1);
    let a_run = &analyst_runs[0];

    // Analyst was triggered with `agent: manager`
    assert_eq!(a_run.trigger, "agent: manager");

    // Analyst produced the expected files and store entries
    assert_eq!(
        rt.kv().get("project.alpha", "summary").unwrap().unwrap().value,
        "metrics analyzed: all good"
    );

    // Check manager's tool call step result: it received a structured HandoffResponse
    let step = mgr_run
        .steps
        .iter()
        .find(|s| matches!(s, Step::Tool { name, .. } if name == "agent:analyst"))
        .unwrap();
    if let Step::Tool { result, error, .. } = step {
        assert!(!error, "tool call should succeed: {result}");
        let resp: HandoffResponse =
            serde_json::from_str(result).expect("must parse as HandoffResponse");
        assert_eq!(resp.status, HandoffStatus::Success);
        assert_eq!(resp.from, "analyst");
        assert_eq!(resp.to, "manager");
        assert!(resp.summary.contains("Analysis successfully completed"));
        // Context produced references were tracked automatically
        assert!(resp
            .context_produced
            .store_keys_written
            .contains(&"project.alpha/summary".to_string()));
        assert!(resp
            .context_produced
            .workspace_files_written
            .contains(&"analysis/report.md".to_string()));
    } else {
        panic!("expected Step::Tool");
    }
}

#[test]
fn explicit_agent_handoff_tool_execution() {
    let rt = runtime("explicit-handoff");

    let worker = agent("worker", &["Done with task."]);
    rt.create_agent(worker).unwrap();

    let call_args = json!({
        "tool": "agent_handoff",
        "args": {
            "target": "worker",
            "intent": "query",
            "task": "Check system status",
            "context": {
                "store_refs": ["system/health"]
            }
        }
    })
    .to_string();

    let mut lead = agent("lead", &[&call_args, "All good."]);
    // Grant agent:worker so lead is authorized to hand off to worker
    lead.capabilities.push(Capability::named("agent:worker"));
    rt.create_agent(lead).unwrap();

    let lead_run =
        rt.run_agent("lead", "http", "check systems", &Cause::default(), "", true).unwrap();
    let step = lead_run
        .steps
        .iter()
        .find(|s| matches!(s, Step::Tool { name, .. } if name == "agent_handoff"))
        .unwrap();
    if let Step::Tool { result, error, .. } = step {
        assert!(!error, "tool call should succeed: {result}");
        let resp: HandoffResponse =
            serde_json::from_str(result).expect("must parse as HandoffResponse");
        assert_eq!(resp.status, HandoffStatus::Success);
        assert_eq!(resp.from, "worker");
        assert_eq!(resp.to, "lead");
        assert_eq!(resp.summary, "Done with task.");
    } else {
        panic!("expected Step::Tool");
    }
}

#[test]
fn legacy_call_agent_remains_backward_compatible() {
    let rt = runtime("legacy-call");

    let worker = agent("worker", &["legacy answer text"]);
    rt.create_agent(worker).unwrap();

    let call_args = json!({
        "tool": "agent:worker",
        "args": {
            "message": "simple legacy message without envelope"
        }
    })
    .to_string();

    let mut caller = agent("caller", &[&call_args, "received answer"]);
    caller.capabilities.push(Capability::named("agent:worker"));
    rt.create_agent(caller).unwrap();

    let caller_run = rt.run_agent("caller", "http", "run", &Cause::default(), "", true).unwrap();
    let step = caller_run
        .steps
        .iter()
        .find(|s| matches!(s, Step::Tool { name, .. } if name == "agent:worker"))
        .unwrap();
    if let Step::Tool { result, error, .. } = step {
        assert!(!error, "tool call should succeed: {result}");
        // Legacy call without envelope gets plain string answer directly
        assert_eq!(result, "legacy answer text");
    } else {
        panic!("expected Step::Tool");
    }
}

#[test]
fn handoff_to_unauthorized_target_is_rejected() {
    let rt = runtime("unauthorized");

    let worker = agent("worker", &["should not run"]);
    rt.create_agent(worker).unwrap();

    let call_args = json!({
        "tool": "agent_handoff",
        "args": {
            "target": "worker",
            "task": "malicious call"
        }
    })
    .to_string();

    // Attacker does NOT hold capability `agent:worker`
    let mut attacker = agent("attacker", &[&call_args, "failed"]);
    attacker.capabilities.push(Capability::named("agent:other")); // only has other
    rt.create_agent(attacker).unwrap();

    let r = rt.run_agent("attacker", "http", "attack", &Cause::default(), "", true).unwrap();
    let step = r
        .steps
        .iter()
        .find(|s| matches!(s, Step::Tool { name, .. } if name == "agent_handoff"))
        .unwrap();
    if let Step::Tool { result, error, .. } = step {
        assert!(error);
        assert!(result
            .contains("you may only hand off to agents you have `agent:<name>` for; not `worker`"));
    } else {
        panic!("expected Step::Tool");
    }
}

#[test]
fn http_v1_handoff_route_handling() {
    let rt = runtime("http-v1-handoff");
    let worker = agent("worker", &["v1 response text"]);
    rt.create_agent(worker).unwrap();

    let body = json!({
        "caller": "gateway",
        "intent": "query",
        "task": "Query status",
        "context": {
            "store_refs": ["shared/status"]
        }
    })
    .to_string();

    let reply = agent_runtime::v1::route(
        &rt,
        &tiny_http::Method::Post,
        &["agents", "worker", "handoff"],
        "",
        &body,
        false, // open tier
        None,
    );

    match reply {
        agent_runtime::v1::Reply::Json(status, val) => {
            assert_eq!(status, 200);
            assert_eq!(val["status"], "success");
            assert_eq!(val["from"], "worker");
            assert_eq!(val["to"], "gateway");
            assert_eq!(val["summary"], "v1 response text");
        }
        other => panic!("expected Reply::Json, got {:?}", other),
    }
}
