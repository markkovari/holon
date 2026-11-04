//! Agent Handoff and Context Routing Protocol (AHRP - ADR-0129).
//!
//! Replaces raw unstructured string messages with structured request/response
//! envelopes and pass-by-reference context pointers (knowledge base, blackboard
//! keys, workspace files, memory digests).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The intent of an agent handoff delegation.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HandoffIntent {
    /// Hand off an end-to-end task / sub-goal to the target agent.
    #[default]
    Delegate,
    /// Query the target agent for specific knowledge or an analysis.
    Query,
    /// Request the target agent review, critique, or verify artifacts/code.
    Review,
    /// Request execution of tools / actions privileged to the target agent.
    Execute,
    /// Hand over turn/conversation state permanently or transitionally.
    Handoff,
}

impl HandoffIntent {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Delegate => "delegate",
            Self::Query => "query",
            Self::Review => "review",
            Self::Execute => "execute",
            Self::Handoff => "handoff",
        }
    }
}

/// Pass-by-reference context pointers to stored knowledge, avoiding prompt token bloat.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextRefs {
    /// Keys in the blackboard KV store (e.g. `["project.alpha/analysis", "shared/schema"]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub store_refs: Vec<String>,
    /// File paths in the workspace or shared code store (e.g. `["reports/q3.md", "src/parser.rs"]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_refs: Vec<String>,
    /// Memory keys, entry IDs, or vector search queries in the agent's knowledge base.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory_refs: Vec<String>,
    /// Correlated run IDs or prior session records.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub run_refs: Vec<String>,
    /// OCI digests, Git blob/tree hashes, or content-addressed artifact identifiers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
}

impl ContextRefs {
    pub fn is_empty(&self) -> bool {
        self.store_refs.is_empty()
            && self.workspace_refs.is_empty()
            && self.memory_refs.is_empty()
            && self.run_refs.is_empty()
            && self.artifacts.is_empty()
    }
}

/// Request envelope for direct agent handoffs and delegations.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct HandoffRequest {
    /// Unique correlation ID for this handoff.
    #[serde(default)]
    pub id: String,
    /// Calling agent name.
    #[serde(default)]
    pub caller: String,
    /// Target agent name.
    #[serde(default)]
    pub target: String,
    /// Intent of this delegation.
    #[serde(default)]
    pub intent: HandoffIntent,
    /// The primary task instruction or question.
    pub task: String,
    /// Context pointers (blackboard, workspace, memory, runs, artifacts).
    #[serde(default)]
    pub context: ContextRefs,
    /// Optional structured parameters for the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
    /// Optional execution budget (e.g. max tokens or steps).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Distributed trace identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Parent span identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
}

impl HandoffRequest {
    pub fn new(
        caller: impl Into<String>,
        target: impl Into<String>,
        task: impl Into<String>,
    ) -> Self {
        Self {
            id: String::new(),
            caller: caller.into(),
            target: target.into(),
            intent: HandoffIntent::Delegate,
            task: task.into(),
            context: ContextRefs::default(),
            parameters: None,
            max_tokens: None,
            trace_id: None,
            parent_span_id: None,
        }
    }

    pub fn with_intent(mut self, intent: HandoffIntent) -> Self {
        self.intent = intent;
        self
    }

    pub fn with_store_ref(mut self, key: impl Into<String>) -> Self {
        self.context.store_refs.push(key.into());
        self
    }

    pub fn with_workspace_ref(mut self, path: impl Into<String>) -> Self {
        self.context.workspace_refs.push(path.into());
        self
    }

    pub fn with_memory_ref(mut self, mem: impl Into<String>) -> Self {
        self.context.memory_refs.push(mem.into());
        self
    }

    pub fn with_parameters(mut self, params: Value) -> Self {
        self.parameters = Some(params);
        self
    }

    /// Renders an ambient markdown section suitable for the target agent's initial prompt turn.
    pub fn to_ambient_prompt(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "Handoff from `{}` (intent: `{}`",
            if self.caller.is_empty() { "external" } else { &self.caller },
            self.intent.as_str()
        ));
        if !self.id.is_empty() {
            out.push_str(&format!(", id: {}", self.id));
        }
        out.push_str(")\n");
        out.push_str(&format!("Task: {}\n", self.task));

        if !self.context.is_empty() {
            out.push_str("\nContext References (stored knowledge):\n");
            if !self.context.store_refs.is_empty() {
                out.push_str(&format!(
                    "- Blackboard store keys: {} (use `store_get` to retrieve)\n",
                    self.context.store_refs.join(", ")
                ));
            }
            if !self.context.workspace_refs.is_empty() {
                out.push_str(&format!(
                    "- Workspace files: {} (use `read_file` to inspect)\n",
                    self.context.workspace_refs.join(", ")
                ));
            }
            if !self.context.memory_refs.is_empty() {
                out.push_str(&format!(
                    "- Memory references: {} (use `recall` to search)\n",
                    self.context.memory_refs.join(", ")
                ));
            }
            if !self.context.run_refs.is_empty() {
                out.push_str(&format!(
                    "- Correlated run IDs: {}\n",
                    self.context.run_refs.join(", ")
                ));
            }
            if !self.context.artifacts.is_empty() {
                out.push_str(&format!("- Artifacts: {}\n", self.context.artifacts.join(", ")));
            }
        }

        if let Some(ref p) = self.parameters {
            out.push_str(&format!("\nParameters: {}\n", p));
        }

        out
    }
}

/// Outcome status of a handoff.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HandoffStatus {
    /// Target successfully completed the task.
    #[default]
    Success,
    /// Target declined or refused the task (e.g. out of scope, policy, missing capability).
    Declined,
    /// Target partially completed the task or reached step limits.
    Partial,
    /// Execution failed with an error.
    Error,
}

impl HandoffStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Declined => "declined",
            Self::Partial => "partial",
            Self::Error => "error",
        }
    }
}

/// Pointers to context produced or modified during the delegated run.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextProduced {
    /// Blackboard KV store keys written or modified.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub store_keys_written: Vec<String>,
    /// Files written or modified in the workspace.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_files_written: Vec<String>,
    /// Artifacts produced or saved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts_produced: Vec<String>,
}

impl ContextProduced {
    pub fn is_empty(&self) -> bool {
        self.store_keys_written.is_empty()
            && self.workspace_files_written.is_empty()
            && self.artifacts_produced.is_empty()
    }
}

/// Response envelope returned from a handoff invocation.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct HandoffResponse {
    /// Unique correlation ID matching the request.
    #[serde(default)]
    pub id: String,
    /// Responding agent name.
    #[serde(default)]
    pub from: String,
    /// Requesting agent name.
    #[serde(default)]
    pub to: String,
    /// Outcome status.
    pub status: HandoffStatus,
    /// Concise summary of the outcome/findings.
    pub summary: String,
    /// References to context produced (blackboard keys, files, artifacts).
    #[serde(default)]
    pub context_produced: ContextProduced,
    /// Optional structured return value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    /// Tokens consumed by the delegated run.
    #[serde(default)]
    pub tokens_spent: u64,
    /// Optional error details if status is `Error` or `Declined`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Sub-delegations performed during this run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delegations: Vec<DelegationRecord>,
}

impl HandoffResponse {
    pub fn success(
        id: impl Into<String>,
        from: impl Into<String>,
        to: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            from: from.into(),
            to: to.into(),
            status: HandoffStatus::Success,
            summary: summary.into(),
            context_produced: ContextProduced::default(),
            output: None,
            tokens_spent: 0,
            error: None,
            delegations: Vec::new(),
        }
    }

    pub fn error(
        id: impl Into<String>,
        from: impl Into<String>,
        to: impl Into<String>,
        err: impl Into<String>,
    ) -> Self {
        let e = err.into();
        Self {
            id: id.into(),
            from: from.into(),
            to: to.into(),
            status: HandoffStatus::Error,
            summary: String::new(),
            context_produced: ContextProduced::default(),
            output: None,
            tokens_spent: 0,
            error: Some(e),
            delegations: Vec::new(),
        }
    }

    pub fn declined(
        id: impl Into<String>,
        from: impl Into<String>,
        to: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        let r = reason.into();
        Self {
            id: id.into(),
            from: from.into(),
            to: to.into(),
            status: HandoffStatus::Declined,
            summary: String::new(),
            context_produced: ContextProduced::default(),
            output: None,
            tokens_spent: 0,
            error: Some(r),
            delegations: Vec::new(),
        }
    }

    /// Renders delegations as a collapsible GitHub-flavored markdown block, if any.
    pub fn delegations_markdown(&self) -> Option<String> {
        if self.delegations.is_empty() {
            None
        } else {
            Some(format_delegations_markdown(&self.delegations))
        }
    }
}

/// Record of a delegation from this agent to another agent during a run.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DelegationRecord {
    /// The target agent that was invoked.
    pub target: String,
    /// Intent of the delegation.
    #[serde(default)]
    pub intent: HandoffIntent,
    /// The task instruction or query passed to the target.
    pub task: String,
    /// Outcome status of the delegation.
    #[serde(default)]
    pub status: HandoffStatus,
    /// Concise summary or answer returned by the target agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Blackboard storage keys written by the target agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub store_keys_written: Vec<String>,
    /// Workspace files written or edited by the target agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_files_written: Vec<String>,
}

impl DelegationRecord {
    pub fn new(
        target: impl Into<String>,
        intent: HandoffIntent,
        task: impl Into<String>,
        status: HandoffStatus,
    ) -> Self {
        Self {
            target: target.into(),
            intent,
            task: task.into(),
            status,
            summary: None,
            store_keys_written: Vec::new(),
            workspace_files_written: Vec::new(),
        }
    }

    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }
}

/// Renders a slice of delegation records as a collapsible GitHub-flavored markdown block.
pub fn format_delegations_markdown(delegations: &[DelegationRecord]) -> String {
    if delegations.is_empty() {
        return String::new();
    }
    let count = delegations.len();
    let label =
        if count == 1 { "1 delegation".to_string() } else { format!("{count} delegations") };
    let mut unique_agents = Vec::new();
    for d in delegations {
        if !unique_agents.contains(&d.target) {
            unique_agents.push(d.target.clone());
        }
    }
    let targets_str = unique_agents.join(", ");

    let mut md = String::new();
    md.push_str(&format!(
        "<details>\n<summary>🤖 <b>Activity ({label}: {targets_str})</b></summary>\n\n"
    ));
    for d in delegations {
        let status_icon = match d.status {
            HandoffStatus::Success => "✓",
            HandoffStatus::Declined => "⚠",
            HandoffStatus::Partial => "⚡",
            HandoffStatus::Error => "✗",
        };
        md.push_str(&format!(
            "- {status_icon} **`{}`** (*{}*): {}\n",
            d.target,
            d.intent.as_str(),
            d.task.trim()
        ));
        if let Some(ref sum) = d.summary {
            let first_line = sum.lines().next().unwrap_or("").trim();
            if !first_line.is_empty() && first_line != d.task.trim() {
                let clipped = if first_line.chars().count() <= 120 {
                    first_line.to_string()
                } else {
                    format!("{}…", first_line.chars().take(120).collect::<String>())
                };
                md.push_str(&format!("  ↳ {clipped}\n"));
            }
        }
        if !d.store_keys_written.is_empty() {
            md.push_str(&format!("  ↳ Stored keys: {}\n", d.store_keys_written.join(", ")));
        }
        if !d.workspace_files_written.is_empty() {
            md.push_str(&format!(
                "  ↳ Workspace files: {}\n",
                d.workspace_files_written.join(", ")
            ));
        }
    }
    md.push_str("\n</details>");
    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_handoff_request_roundtrip() {
        let mut req = HandoffRequest::new("researcher", "analyst", "Analyze quarterly trends")
            .with_intent(HandoffIntent::Query)
            .with_store_ref("project.alpha/metrics")
            .with_workspace_ref("reports/q3.csv")
            .with_memory_ref("financial_baseline")
            .with_parameters(json!({"granularity": "monthly"}));
        req.id = "req-123".into();

        let s = serde_json::to_string(&req).unwrap();
        let de: HandoffRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(de.id, "req-123");
        assert_eq!(de.caller, "researcher");
        assert_eq!(de.target, "analyst");
        assert_eq!(de.intent, HandoffIntent::Query);
        assert_eq!(de.task, "Analyze quarterly trends");
        assert_eq!(de.context.store_refs, vec!["project.alpha/metrics"]);
        assert_eq!(de.context.workspace_refs, vec!["reports/q3.csv"]);
        assert_eq!(de.context.memory_refs, vec!["financial_baseline"]);
        assert_eq!(de.parameters, Some(json!({"granularity": "monthly"})));

        let prompt = req.to_ambient_prompt();
        assert!(prompt.contains("Handoff from `researcher` (intent: `query`, id: req-123)"));
        assert!(prompt.contains("Blackboard store keys: project.alpha/metrics"));
        assert!(prompt.contains("Workspace files: reports/q3.csv"));
    }

    #[test]
    fn test_handoff_response_roundtrip() {
        let mut res =
            HandoffResponse::success("req-123", "analyst", "researcher", "Analysis complete.");
        res.tokens_spent = 450;
        res.context_produced.store_keys_written.push("project.alpha/summary".into());
        res.context_produced.workspace_files_written.push("reports/q3_analyzed.md".into());

        let s = serde_json::to_string(&res).unwrap();
        let de: HandoffResponse = serde_json::from_str(&s).unwrap();
        assert_eq!(de.id, "req-123");
        assert_eq!(de.status, HandoffStatus::Success);
        assert_eq!(de.summary, "Analysis complete.");
        assert_eq!(de.tokens_spent, 450);
        assert_eq!(de.context_produced.store_keys_written, vec!["project.alpha/summary"]);
        assert_eq!(de.context_produced.workspace_files_written, vec!["reports/q3_analyzed.md"]);
        assert!(de.error.is_none());
    }

    #[test]
    fn test_delegation_record_and_markdown() {
        let mut d1 = DelegationRecord::new(
            "data-extractor",
            HandoffIntent::Query,
            "Extract raw billing records",
            HandoffStatus::Success,
        )
        .with_summary("Extracted 42 records from q3.csv");
        d1.workspace_files_written.push("reports/q3.csv".into());

        let d2 = DelegationRecord::new(
            "auditor",
            HandoffIntent::Review,
            "Verify compliance with ASC 606",
            HandoffStatus::Success,
        )
        .with_summary("All checks passed; 0 violations");

        let delegations = vec![d1, d2];
        let md = format_delegations_markdown(&delegations);
        assert!(md.contains("<details>"));
        assert!(md.contains(
            "<summary>🤖 <b>Activity (2 delegations: data-extractor, auditor)</b></summary>"
        ));
        assert!(md.contains("- ✓ **`data-extractor`** (*query*): Extract raw billing records"));
        assert!(md.contains("↳ Extracted 42 records from q3.csv"));
        assert!(md.contains("↳ Workspace files: reports/q3.csv"));
        assert!(md.contains("- ✓ **`auditor`** (*review*): Verify compliance with ASC 606"));
        assert!(md.contains("↳ All checks passed; 0 violations"));
        assert!(md.ends_with("</details>"));
    }
}
