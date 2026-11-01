use std::sync::Arc;
use std::time::Duration;

use agent_runtime::agent::{compute_cost_micros, Cause, Event};
use agent_runtime::projects::Project;
use agent_runtime::store::{CostLedgerEntry, Status, Step};
use agent_runtime::{otlp, AgentSpec, Capability, Config, ModelSpec, Runtime, Trigger};
use serde_json::json;

fn dir(tag: &str) -> std::path::PathBuf {
    tempfile::Builder::new().prefix(&format!("ar-gov-{tag}-")).tempdir().unwrap().keep()
}

fn test_runtime(tag: &str) -> Arc<Runtime> {
    let mut cfg = Config::new(dir(tag));
    cfg.breaker_threshold = 2;
    cfg.breaker_cooldown = Duration::from_secs(60);
    Runtime::new(cfg).unwrap()
}

fn add_mock_agent(rt: &Runtime, name: &str, replies: Vec<String>, caps: Vec<&str>) -> AgentSpec {
    let mut s = AgentSpec::new(name, "test agent");
    s.capabilities = caps.into_iter().map(Capability::named).collect();
    s.model = ModelSpec::Mock { replies };
    rt.create_agent(s.clone()).unwrap();
    s
}

#[test]
fn test_cost_attribution_and_multi_tenant_ledger() {
    // 1. Verify cost calculation logic across model tiers
    assert_eq!(compute_cost_micros("mock", 1000, 1000), 0);
    assert_eq!(compute_cost_micros("local/model", 1000, 1000), 0);
    // Haiku: 250 in, 1250 out per 1k tokens
    assert_eq!(compute_cost_micros("anthropic/claude-3-5-haiku", 1000, 1000), 1500);
    // Sonnet: 3000 in, 15000 out per 1k tokens
    assert_eq!(compute_cost_micros("anthropic/claude-3-5-sonnet", 1000, 1000), 18000);
    // Deepseek: 140 in, 280 out per 1k tokens
    assert_eq!(compute_cost_micros("deepseek/deepseek-chat", 1000, 1000), 420);

    let rt = test_runtime("cost");

    // 2. Test Store cost recording and ledger retrieval
    let now_ms = (agent_runtime::runtime::unix_now() - 3600) * 1000;
    let entry1 = CostLedgerEntry {
        id: "run-1".into(),
        agent: "finance-bot".into(),
        tenant: "tenant_alpha".into(),
        project: "budgeting".into(),
        model: "anthropic/claude-3-5-sonnet".into(),
        tokens_in: 2000,
        tokens_out: 500,
        cost_micros: 13500,
        timestamp_ms: now_ms,
    };
    let entry2 = CostLedgerEntry {
        id: "run-2".into(),
        agent: "finance-bot".into(),
        tenant: "tenant_alpha".into(),
        project: "budgeting".into(),
        model: "anthropic/claude-3-5-sonnet".into(),
        tokens_in: 1000,
        tokens_out: 200,
        cost_micros: 6000,
        timestamp_ms: now_ms + 1000,
    };
    let entry3 = CostLedgerEntry {
        id: "run-3".into(),
        agent: "support-bot".into(),
        tenant: "tenant_beta".into(),
        project: "triage".into(),
        model: "deepseek/deepseek-chat".into(),
        tokens_in: 5000,
        tokens_out: 1000,
        cost_micros: 980,
        timestamp_ms: now_ms + 2000,
    };

    rt.store().record_cost(&entry1).unwrap();
    rt.store().record_cost(&entry2).unwrap();
    rt.store().record_cost(&entry3).unwrap();

    let finance_costs = rt.store().cost_ledger("finance-bot", 0);
    assert_eq!(finance_costs.len(), 2);
    assert_eq!(finance_costs[0].id, "run-1");
    assert_eq!(finance_costs[1].id, "run-2");

    let alpha_costs = rt.store().cost_ledger_for_tenant("tenant_alpha", 0);
    assert_eq!(alpha_costs.len(), 2);
    let beta_costs = rt.store().cost_ledger_for_tenant("tenant_beta", 0);
    assert_eq!(beta_costs.len(), 1);
    assert_eq!(beta_costs[0].agent, "support-bot");

    // 3. Test cost_ledger agent tool invocation
    let mut audit_agent = AgentSpec::new("auditor", "Audit inspector");
    audit_agent.capabilities = vec![Capability::named("cost_ledger")];
    audit_agent.auto_approve = vec!["cost_ledger".into()];
    audit_agent.model = ModelSpec::Mock {
        replies: vec![
            r#"{"tool": "cost_ledger", "args": {"agent": "finance-bot", "since_hours": 48}}"#
                .into(),
            "Audit complete.".into(),
        ],
    };
    rt.create_agent(audit_agent).unwrap();

    let rec = rt.run_agent("auditor", "ui", "Run audit", &Cause::default(), "", true).unwrap();
    assert_eq!(rec.status, Status::Ok);
    let tool_step = rec.steps.iter().find(|s| match s {
        Step::Tool { name, .. } => name == "cost_ledger",
        _ => false,
    });
    assert!(tool_step.is_some());
    if let Some(Step::Tool { result, .. }) = tool_step {
        let parsed: serde_json::Value = serde_json::from_str(result).unwrap();
        assert_eq!(parsed["query_agent"], "finance-bot");
        assert_eq!(parsed["entry_count"], 2);
        assert_eq!(parsed["total_tokens"], 3700);
        assert_eq!(parsed["total_cost_micros"], 19500);
    }
}

#[test]
fn test_opentelemetry_traces_and_w3c_context() {
    let rt = test_runtime("otlp");

    add_mock_agent(
        &rt,
        "worker",
        vec![
            r#"{"tool": "remember", "args": {"text": "trace sample"}}"#.into(),
            "Remembered trace sample.".into(),
        ],
        vec!["remember"],
    );

    let proj = Project {
        name: "project-nebula".into(),
        description: "Nebula".into(),
        agents: vec!["worker".into()],
        ..Default::default()
    };
    rt.put_project(proj).unwrap();

    let cause = Cause {
        tenant: Some("acme-global".into()),
        project: Some("project-nebula".into()),
        ..Cause::default()
    };

    let rec = rt.run_agent("worker", "ui", "Do work", &cause, "", true).unwrap();
    assert_eq!(rec.status, Status::Ok);
    assert_eq!(rec.tenant, "acme-global");
    assert_eq!(rec.project, "project-nebula");

    // Build OTLP trace export
    let otlp_doc = otlp::request(std::slice::from_ref(&rec));
    let spans = otlp_doc["resourceSpans"][0]["scopeSpans"][0]["spans"].as_array().unwrap();
    assert!(spans.len() >= 3); // invoke_agent, chat, execute_tool

    let root_span = &spans[0];
    assert_eq!(root_span["name"], "invoke_agent worker");
    let attrs = root_span["attributes"].as_array().unwrap();

    let get_attr = |key: &str| -> Option<String> {
        attrs.iter().find(|a| a["key"] == key).map(|a| {
            let v = &a["value"];
            v["stringValue"].as_str().or_else(|| v["intValue"].as_str()).unwrap().to_string()
        })
    };

    assert_eq!(get_attr("holon.tenant").as_deref(), Some("acme-global"));
    assert_eq!(get_attr("holon.project").as_deref(), Some("project-nebula"));
    assert!(get_attr("w3c.traceparent").is_some());
    assert!(get_attr("w3c.traceparent").unwrap().starts_with("00-"));

    // Check tool span status
    let tool_span =
        spans.iter().find(|s| s["name"].as_str().unwrap().starts_with("execute_tool")).unwrap();
    let tool_attrs = tool_span["attributes"].as_array().unwrap();
    let tool_status = tool_attrs.iter().find(|a| a["key"] == "holon.step.status").unwrap();
    assert_eq!(tool_status["value"]["stringValue"], "ok");
}

#[test]
fn test_watchdog_alarms_and_proactive_diagnostics() {
    let rt = test_runtime("watchdog");
    let rx = rt.subscribe();

    // 1. Circuit breaker trip alarm
    let mut fail_agent = AgentSpec::new("breaker-agent", "Agent that trips breaker");
    fail_agent.model = ModelSpec::Mock { replies: vec![] }; // Empty replies -> Error/failure
    rt.create_agent(fail_agent).unwrap();

    // Run twice to hit breaker_threshold = 2
    let _ = rt.run_agent("breaker-agent", "ui", "run 1", &Cause::default(), "", true);
    let _ = rt.run_agent("breaker-agent", "ui", "run 2", &Cause::default(), "", true);

    let mut alarms = Vec::new();
    while let Ok(e) = rx.try_recv() {
        if let Event::WatchdogAlarm { alarm, details, severity, agent, .. } = e {
            alarms.push((agent, alarm, details, severity));
        }
    }

    assert!(alarms.iter().any(|(agent, alarm, _, severity)| {
        agent == "breaker-agent" && alarm == "circuit_breaker_opened" && severity == "critical"
    }));

    // 2. Near-budget and budget-exceeded alarm
    let mut budget_agent = AgentSpec::new("budget-agent", "Agent with daily budget");
    budget_agent.daily_token_budget = 1000;
    budget_agent.model = ModelSpec::Mock { replies: vec!["Done.".into()] };
    rt.create_agent(budget_agent).unwrap();

    let now = agent_runtime::runtime::unix_now();

    // Create a run in store that consumed 850 tokens within last 24h
    let rec = agent_runtime::store::RunRecord {
        id: "budget-run-1".into(),
        agent: "budget-agent".into(),
        trigger: "ui".into(),
        input: "hi".into(),
        started: now - 60,
        finished: now - 10,
        status: Status::Ok,
        answer: "done".into(),
        steps: vec![],
        tokens_in: 700,
        tokens_out: 150,
        started_ms: (now - 60) * 1000,
        finished_ms: (now - 10) * 1000,
        model: "mock".into(),
        trace_id: "t".into(),
        span_id: "s".into(),
        parent_span_id: None,
        hops: 0,
        chain: vec![],
        project: "".into(),
        attachments: vec![],
        cost_micros: 0,
        tenant: "".into(),
    };
    rt.store().record_run(&rec).unwrap();

    // Next run should trigger near_budget_threshold alarm
    let _ = rt.run_agent("budget-agent", "ui", "test budget", &Cause::default(), "", true);
    while let Ok(e) = rx.try_recv() {
        if let Event::WatchdogAlarm { alarm, details, severity, agent, .. } = e {
            alarms.push((agent, alarm, details, severity));
        }
    }
    assert!(alarms.iter().any(|(agent, alarm, _, severity)| {
        agent == "budget-agent" && alarm == "near_budget_threshold" && severity == "warning"
    }));

    // 3. Heartbeat diagnostics test
    let diag_alarms = rt.check_heartbeat_diagnostics(now);
    assert!(!diag_alarms.is_empty());
}

#[test]
fn test_stuck_execution_loop_detection() {
    let rt = test_runtime("loop");
    let rx = rt.subscribe();

    // Agent invokes the exact same tool call 3 times in a row
    let loop_call = r#"{"tool": "store_get", "args": {"ns": "shared", "key": "locked_key"}}"#;
    let mut loop_agent = AgentSpec::new("loop-agent", "Agent caught in tool loop");
    loop_agent.capabilities = vec![Capability::named("store_get")];
    loop_agent.auto_approve = vec!["store_get".into()];
    loop_agent.model = ModelSpec::Mock {
        replies: vec![
            loop_call.into(),
            loop_call.into(),
            loop_call.into(),
            "Breaking loop.".into(),
        ],
    };
    rt.create_agent(loop_agent).unwrap();

    let rec = rt.run_agent("loop-agent", "ui", "Check lock", &Cause::default(), "", true).unwrap();
    assert_eq!(rec.status, Status::Ok);

    let mut alarms = Vec::new();
    while let Ok(e) = rx.try_recv() {
        if let Event::WatchdogAlarm { alarm, details, severity, agent, .. } = e {
            alarms.push((agent, alarm, details, severity));
        }
    }

    assert!(alarms.iter().any(|(agent, alarm, _, severity)| {
        agent == "loop-agent" && alarm == "stuck_execution_loop" && severity == "warning"
    }));
}

#[test]
fn test_propose_and_apply_spec_patch() {
    let rt = test_runtime("patch");
    let rx = rt.subscribe();

    // Create target agent to improve
    let target = AgentSpec::new("target-bot", "Original description");
    rt.create_agent(target).unwrap();

    // Create curator agent with agent_propose_spec_patch and agent_apply_spec_patch
    let mut curator = AgentSpec::new("curator-bot", "Agent Curator");
    curator.capabilities = vec![
        Capability::named("agent_propose_spec_patch"),
        Capability::named("agent_apply_spec_patch"),
        Capability::named("agent_curator"),
    ];
    curator.auto_approve = vec!["agent_propose_spec_patch".into(), "agent_apply_spec_patch".into()];
    curator.model = ModelSpec::Mock {
        replies: vec![
            json!({
                "tool": "agent_propose_spec_patch",
                "args": {
                    "target_agent": "target-bot",
                    "patch": serde_json::to_string(&json!({
                        "description": "Upgraded system prompt for target_bot",
                        "add_capabilities": ["http_get"],
                        "schedule": "@hourly",
                        "schedule_prompt": "Perform hourly health check"
                    })).unwrap(),
                    "reason": "Target bot requires net connectivity and proactive hourly checks."
                }
            })
            .to_string(),
            "Proposal submitted.".into(),
        ],
    };
    rt.create_agent(curator).unwrap();

    let rec = rt
        .run_agent("curator-bot", "ui", "Improve target-bot", &Cause::default(), "", true)
        .unwrap();
    assert_eq!(rec.status, Status::Ok, "curator run failed: {}", rec.answer);

    // Verify Event::SpecPatchProposed was emitted
    let mut patch_proposals = Vec::new();
    while let Ok(e) = rx.try_recv() {
        if let Event::SpecPatchProposed(p) = e {
            patch_proposals.push(p);
        }
    }
    assert_eq!(patch_proposals.len(), 1);
    let proposal_id = patch_proposals[0].proposal_id.clone();
    assert_eq!(patch_proposals[0].agent, "target-bot");
    assert_eq!(patch_proposals[0].proposer, "curator-bot");
    assert_eq!(patch_proposals[0].status, "pending_review");

    // Verify proposal is stored in store
    let patches = rt.store().spec_patches("target-bot");
    assert_eq!(patches.len(), 1);
    assert_eq!(patches[0].proposal_id, proposal_id);

    // Apply the spec patch
    let mut applier = AgentSpec::new("reviewer-bot", "Patch reviewer");
    applier.capabilities =
        vec![Capability::named("agent_apply_spec_patch"), Capability::named("agent_curator")];
    applier.auto_approve = vec!["agent_apply_spec_patch".into()];
    applier.model = ModelSpec::Mock {
        replies: vec![
            json!({
                "tool": "agent_apply_spec_patch",
                "args": {
                    "target_agent": "target-bot",
                    "proposal_id": proposal_id
                }
            })
            .to_string(),
            "Patch applied.".into(),
        ],
    };
    rt.create_agent(applier).unwrap();

    let apply_rec =
        rt.run_agent("reviewer-bot", "ui", "Apply patch", &Cause::default(), "", true).unwrap();
    assert_eq!(apply_rec.status, Status::Ok, "reviewer run failed: {}", apply_rec.answer);

    // Check target-bot's updated specification
    let updated_target = rt.store().get("target-bot").unwrap();
    assert_eq!(updated_target.description, "Upgraded system prompt for target_bot");
    assert!(updated_target.has_capability("http_get"));
    assert!(updated_target.triggers.iter().any(|t| match t {
        Trigger::Schedule { cron, prompt, .. } =>
            cron == "@hourly" && prompt == "Perform hourly health check",
        _ => false,
    }));
}
