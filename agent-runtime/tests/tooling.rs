//! Integration tests for Pillar III: Tooling, Sandboxing & Runtime Execution.
//!
//! Tests:
//! 1. Dynamic capability/tool discovery (`tools_discover`)
//! 2. Ephemeral workspace sandboxes with CoW branching (`workspace_branch`, `workspace_commit`, `workspace_discard`)
//! 3. Bounded sandboxed command execution (`exec_sandboxed`)
//! 4. Token-bucket rate limiting and backpressure per tool group (`rate_limits`)

use std::sync::Arc;

use agent_runtime::agent::{Cause, DynamicToolDef, Host, ToolGroup, ToolHandler};
use agent_runtime::store::{Status, Step};
use agent_runtime::{AgentSpec, Capability, Config, ModelSpec, Runtime};
use serde_json::Value;

fn dir(tag: &str) -> std::path::PathBuf {
    tempfile::Builder::new().prefix(&format!("ar-tooling-{tag}-")).tempdir().unwrap().keep()
}

fn runtime(tag: &str) -> Arc<Runtime> {
    Runtime::new(Config::new(dir(tag))).unwrap()
}

fn agent(name: &str, replies: &[&str]) -> AgentSpec {
    let mut s = AgentSpec::new(name, "a tooling test agent");
    s.model = ModelSpec::Mock { replies: replies.iter().map(|r| r.to_string()).collect() };
    s
}

struct DummyCustomHandler;

impl ToolHandler for DummyCustomHandler {
    fn can_handle(&self, name: &str) -> bool {
        name == "wasm_calculate"
    }

    fn execute(
        &self,
        _host: &dyn Host,
        _name: &str,
        _args: &Value,
        _cause: &Cause,
    ) -> Result<String, String> {
        Ok("42".into())
    }

    fn describe_tools(&self) -> Vec<DynamicToolDef> {
        vec![DynamicToolDef {
            name: "wasm_calculate".into(),
            args: r#"{"expr": "6 * 7"}"#.into(),
            about: "Execute math calculation in WASM container".into(),
            category: "exec".into(),
            sensitive: false,
        }]
    }
}

#[test]
fn test_tools_discover_filters_by_category_and_prefix_and_merges_custom_tools() {
    let rt = runtime("discover");
    rt.register_tool_handler(Arc::new(DummyCustomHandler));

    let mut a = agent(
        "discoverer",
        &[
            r#"{"tool":"tools_discover","args":{"category":"workspace"}}"#,
            r#"{"tool":"tools_discover","args":{"category":"exec"}}"#,
            "discovery complete",
        ],
    );
    a.capabilities.push(Capability::named("tools_discover"));
    rt.create_agent(a).unwrap();

    let r = rt.run_agent("discoverer", "http", "go", &Cause::default(), "", true).unwrap();
    assert_eq!(r.status, Status::Ok);

    let steps: Vec<_> = r
        .steps
        .iter()
        .filter_map(|s| match s {
            Step::Tool { name, result, .. } if name == "tools_discover" => Some(result.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(steps.len(), 2);

    // Step 0: category "workspace"
    let ws_tools: Vec<DynamicToolDef> = serde_json::from_str(&steps[0]).unwrap();
    let ws_names: Vec<_> = ws_tools.iter().map(|t| t.name.as_str()).collect();
    assert!(ws_names.contains(&"workspace_branch"));
    assert!(ws_names.contains(&"workspace_commit"));
    assert!(ws_names.contains(&"workspace_discard"));
    assert!(!ws_names.contains(&"exec_sandboxed"));

    // Step 1: category "exec" (should include built-in exec_sandboxed, session_start, and dynamic wasm_calculate)
    let exec_tools: Vec<DynamicToolDef> = serde_json::from_str(&steps[1]).unwrap();
    let exec_names: Vec<_> = exec_tools.iter().map(|t| t.name.as_str()).collect();
    assert!(exec_names.contains(&"exec_sandboxed"));
    assert!(exec_names.contains(&"wasm_calculate"));
    assert!(!exec_names.contains(&"workspace_branch"));
}

#[test]
fn test_workspace_branch_isolation_commit_and_discard() {
    let rt = runtime("ws-branch");
    let mut a = agent(
        "branch-agent",
        &[
            // 1. Write file in main workspace
            r#"{"tool":"write_file","args":{"path":"base.txt","content":"v1 main"}}"#,
            // 2. Branch workspace
            r#"{"tool":"workspace_branch","args":{"branch":"feat-x","action":"create"}}"#,
            // 3. Edit base.txt in branch
            r#"{"tool":"edit_file","args":{"path":"base.txt","target":"v1 main","replacement":"v2 branch","branch":"feat-x"}}"#,
            // 4. Write new file in branch
            r#"{"tool":"write_file","args":{"path":"new.txt","content":"brand new in branch","branch":"feat-x"}}"#,
            // 5. Read base.txt from main workspace (must be untouched!)
            r#"{"tool":"read_file","args":{"path":"base.txt"}}"#,
            // 6. Read base.txt from branch (must be changed)
            r#"{"tool":"read_file","args":{"path":"base.txt","branch":"feat-x"}}"#,
            // 7. Commit branch
            r#"{"tool":"workspace_commit","args":{"branch":"feat-x"}}"#,
            // 8. Read base.txt from main workspace (must now reflect the branch change)
            r#"{"tool":"read_file","args":{"path":"base.txt"}}"#,
            // 9. Read new.txt from main workspace (must now exist)
            r#"{"tool":"read_file","args":{"path":"new.txt"}}"#,
            "branching verified",
        ],
    );
    a.capabilities.push(Capability::named("read_file"));
    a.capabilities.push(Capability::named("write_file"));
    a.capabilities.push(Capability::named("edit_file"));
    a.capabilities.push(Capability::named("workspace_branch"));
    a.capabilities.push(Capability::named("workspace_commit"));
    a.capabilities.push(Capability::named("workspace_discard"));
    a.max_steps = 15;
    a.auto_approve = vec![
        "write_file".into(),
        "edit_file".into(),
        "workspace_commit".into(),
        "workspace_discard".into(),
    ];
    rt.create_agent(a).unwrap();

    let r = rt.run_agent("branch-agent", "http", "go", &Cause::default(), "", true).unwrap();
    assert_eq!(r.status, Status::Ok);

    let tool_results: Vec<(&str, &str)> = r
        .steps
        .iter()
        .filter_map(|s| match s {
            Step::Tool { name, result, .. } => Some((name.as_str(), result.as_str())),
            _ => None,
        })
        .collect();

    // Check step 4 (read main base.txt before commit)
    assert_eq!(tool_results[4].1, "v1 main");
    // Check step 5 (read branch base.txt)
    assert_eq!(tool_results[5].1, "v2 branch");
    // Check step 7 (read main base.txt after commit)
    assert_eq!(tool_results[7].1, "v2 branch");
    // Check step 8 (read main new.txt after commit)
    assert_eq!(tool_results[8].1, "brand new in branch");

    // Also verify discard
    let mut d = agent(
        "discard-agent",
        &[
            r#"{"tool":"workspace_branch","args":{"branch":"feat-temp"}}"#,
            r#"{"tool":"write_file","args":{"path":"discard.txt","content":"will be gone","branch":"feat-temp"}}"#,
            r#"{"tool":"workspace_discard","args":{"branch":"feat-temp"}}"#,
            r#"{"tool":"read_file","args":{"path":"discard.txt"}}"#,
            "discard verified",
        ],
    );
    d.capabilities = vec![
        Capability::named("read_file"),
        Capability::named("write_file"),
        Capability::named("workspace_branch"),
        Capability::named("workspace_discard"),
    ];
    d.auto_approve = vec!["write_file".into(), "workspace_discard".into()];
    rt.create_agent(d).unwrap();

    let rd = rt.run_agent("discard-agent", "http", "go", &Cause::default(), "", true).unwrap();
    assert_eq!(rd.status, Status::Ok);
    let last_read = rd
        .steps
        .iter()
        .filter_map(|s| match s {
            Step::Tool { name, error, .. } if name == "read_file" => Some(*error),
            _ => None,
        })
        .next_back()
        .unwrap();
    assert!(last_read, "discard.txt should not exist in main workspace");
}

#[test]
fn test_exec_sandboxed_bounded_execution_timeout_and_output_truncation() {
    let rt = runtime("exec-sandboxed");
    let mut a = agent(
        "runner",
        &[
            // 1. Basic command output
            r#"{"tool":"exec_sandboxed","args":{"cmd":"echo 'hello holon'"}}"#,
            // 2. Command with timeout limit
            r#"{"tool":"exec_sandboxed","args":{"cmd":"sleep 2","timeout_ms":100}}"#,
            // 3. Command with max output limit truncation
            r#"{"tool":"exec_sandboxed","args":{"cmd":"echo 'abcdefghijklmnopqrstuvwxyz'","max_output_bytes":16}}"#,
            "exec verified",
        ],
    );
    a.capabilities.push(Capability::named("exec_sandboxed"));
    a.auto_approve = vec!["exec_sandboxed".into()];
    rt.create_agent(a).unwrap();

    let r = rt.run_agent("runner", "http", "go", &Cause::default(), "", true).unwrap();
    assert_eq!(r.status, Status::Ok);

    let exec_steps: Vec<Value> = r
        .steps
        .iter()
        .filter_map(|s| match s {
            Step::Tool { name, result, .. } if name == "exec_sandboxed" => {
                Some(serde_json::from_str(result).unwrap())
            }
            _ => None,
        })
        .collect();

    assert_eq!(exec_steps.len(), 3);

    // 1. Basic command
    assert_eq!(exec_steps[0]["exit_code"], 0);
    assert!(exec_steps[0]["stdout"].as_str().unwrap().contains("hello holon"));
    assert_eq!(exec_steps[0]["timed_out"], false);

    // 2. Timeout
    assert_eq!(exec_steps[1]["timed_out"], true);

    // 3. Truncation
    assert_eq!(exec_steps[2]["stdout_truncated"], true);
    assert!(exec_steps[2]["stdout"].as_str().unwrap().contains("[... output truncated ...]"));
}

#[test]
fn test_rate_limiting_tool_groups_with_backpressure() {
    let rt = runtime("rate-limit");
    // Restrict Exec group for rate-agent to 1 token capacity, 0.001 refill/s (burst 1, slow refill)
    rt.set_tool_rate_limit("rate-agent", ToolGroup::Exec, 1.0, 0.001);

    let mut a = agent(
        "rate-agent",
        &[
            // First exec succeeds
            r#"{"tool":"exec_sandboxed","args":{"cmd":"echo first"}}"#,
            // Second exec immediately is rejected by rate limiter
            r#"{"tool":"exec_sandboxed","args":{"cmd":"echo second"}}"#,
            "rate limit handled",
        ],
    );
    a.capabilities.push(Capability::named("exec_sandboxed"));
    a.auto_approve = vec!["exec_sandboxed".into()];
    rt.create_agent(a).unwrap();

    let r = rt.run_agent("rate-agent", "http", "go", &Cause::default(), "", true).unwrap();
    assert_eq!(r.status, Status::Ok);

    let steps: Vec<(&str, &str, bool)> = r
        .steps
        .iter()
        .filter_map(|s| match s {
            Step::Tool { name, result, error, .. } => {
                Some((name.as_str(), result.as_str(), *error))
            }
            _ => None,
        })
        .collect();

    assert_eq!(steps.len(), 2);
    // First call succeeded
    assert_eq!(steps[0].0, "exec_sandboxed");
    assert!(!steps[0].2, "first call should not be an error");

    // Second call failed with rate limit backpressure
    assert_eq!(steps[1].0, "exec_sandboxed");
    assert!(steps[1].2, "second call should be an error");
    assert!(
        steps[1].1.contains("rate limit exceeded for tool group `exec`"),
        "expected rate limit message, got: {}",
        steps[1].1
    );
}
