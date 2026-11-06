//! Effectiveness measurement for the agent harness: failure taxonomy, component reuse ratio,
//! generation yield, and PR success tracking (ADR-0081, ADR-0091).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FailureCategory {
    CompileError,
    GateCheckFailed,
    ScorePlateau,
    TimeoutOrExhausted,
    ContractMismatch,
    ForgeConflict,
    Unknown,
}

impl FailureCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CompileError => "CompileError",
            Self::GateCheckFailed => "GateCheckFailed",
            Self::ScorePlateau => "ScorePlateau",
            Self::TimeoutOrExhausted => "TimeoutOrExhausted",
            Self::ContractMismatch => "ContractMismatch",
            Self::ForgeConflict => "ForgeConflict",
            Self::Unknown => "Unknown",
        }
    }
}

/// Classify failure reasons from error strings, compiler diagnostics, and gate notes.
pub fn classify_failure(raw: &str) -> FailureCategory {
    let lower = raw.to_lowercase();
    if lower.contains("error[e")
        || lower.contains("syntax error")
        || lower.contains("failed to compile")
        || lower.contains("could not compile")
        || lower.contains("cannot find")
    {
        FailureCategory::CompileError
    } else if lower.contains("gate")
        || lower.contains("check failed")
        || lower.contains("assertion failed")
        || lower.contains("test failed")
        || lower.contains("score")
    {
        FailureCategory::GateCheckFailed
    } else if lower.contains("plateau")
        || lower.contains("noprogress")
        || lower.contains("no progress")
    {
        FailureCategory::ScorePlateau
    } else if lower.contains("timeout")
        || lower.contains("exhausted")
        || lower.contains("overbudget")
        || lower.contains("over budget")
    {
        FailureCategory::TimeoutOrExhausted
    } else if lower.contains("contract")
        || lower.contains("wit")
        || lower.contains("schema mismatch")
    {
        FailureCategory::ContractMismatch
    } else if lower.contains("conflict")
        || lower.contains("forge")
        || lower.contains("branch exists")
    {
        FailureCategory::ForgeConflict
    } else {
        FailureCategory::Unknown
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GoalRunMetric {
    pub goal_id: String,
    pub title: String,
    pub accepted: bool,
    pub attempts: usize,
    pub score: u64,
    pub spent_tokens: u64,
    pub stop_reason: String,
    pub failures: Vec<String>,
    pub components_reused: usize,
    pub components_written: usize,
    pub pr_opened: bool,
    pub pr_merged: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EffectivenessSummary {
    pub total_goals: usize,
    pub accepted_goals: usize,
    pub refuted_goals: usize,
    pub exhausted_goals: usize,
    pub plateaued_goals: usize,
    pub yield_percentage: f64,

    pub prs_opened: usize,
    pub prs_merged: usize,
    pub pr_success_rate: f64,

    pub components_reused: usize,
    pub components_written: usize,
    pub component_reuse_ratio: f64,

    pub total_tokens_spent: u64,
    pub failure_distribution: BTreeMap<String, usize>,
}

impl EffectivenessSummary {
    pub fn compute(runs: &[GoalRunMetric]) -> Self {
        let mut summary = Self { total_goals: runs.len(), ..Default::default() };

        for r in runs {
            if r.accepted {
                summary.accepted_goals += 1;
            } else {
                let stop = r.stop_reason.to_lowercase();
                if stop.contains("plateau") || stop.contains("noprogress") {
                    summary.plateaued_goals += 1;
                } else if stop.contains("exhaust") || stop.contains("budget") {
                    summary.exhausted_goals += 1;
                } else {
                    summary.refuted_goals += 1;
                }
            }

            if r.pr_opened {
                summary.prs_opened += 1;
                if r.pr_merged {
                    summary.prs_merged += 1;
                }
            }

            summary.components_reused += r.components_reused;
            summary.components_written += r.components_written;
            summary.total_tokens_spent += r.spent_tokens;

            for f in &r.failures {
                let cat = classify_failure(f);
                *summary.failure_distribution.entry(cat.as_str().to_string()).or_default() += 1;
            }
        }

        if summary.total_goals > 0 {
            summary.yield_percentage =
                (summary.accepted_goals as f64 / summary.total_goals as f64) * 100.0;
        }

        if summary.prs_opened > 0 {
            summary.pr_success_rate =
                (summary.prs_merged as f64 / summary.prs_opened as f64) * 100.0;
        }

        let total_components = summary.components_reused + summary.components_written;
        if total_components > 0 {
            summary.component_reuse_ratio =
                (summary.components_reused as f64 / total_components as f64) * 100.0;
        }

        summary
    }

    pub fn format_report(&self) -> String {
        let mut out = String::new();
        out.push_str("\n=== AGENT & HARNESS EFFECTIVENESS REPORT ===\n\n");
        out.push_str(&format!(
            "  GENERATION YIELD:    {}/{} accepted ({:.1}% yield)\n",
            self.accepted_goals, self.total_goals, self.yield_percentage
        ));
        out.push_str(&format!(
            "                       {} plateaued  ·  {} exhausted  ·  {} refuted\n",
            self.plateaued_goals, self.exhausted_goals, self.refuted_goals
        ));
        out.push_str(&format!(
            "  PULL REQUESTS:       {}/{} merged ({:.1}% PR landing rate)\n",
            self.prs_merged, self.prs_opened, self.pr_success_rate
        ));
        out.push_str(&format!(
            "  COMPONENT REUSE:     reused {}  ·  written {}  ({:.1}% reuse ratio)\n",
            self.components_reused, self.components_written, self.component_reuse_ratio
        ));
        out.push_str(&format!("  TOTAL TOKEN SPEND:   {} tokens\n\n", self.total_tokens_spent));

        out.push_str("  FAILURE TAXONOMY:\n");
        if self.failure_distribution.is_empty() {
            out.push_str("    no failures recorded across evaluated runs\n");
        } else {
            let total_failures: usize = self.failure_distribution.values().sum();
            for (cat, count) in &self.failure_distribution {
                let pct = (*count as f64 / total_failures as f64) * 100.0;
                out.push_str(&format!("    - {:<20} {:>4} ({:.1}%)\n", cat, count, pct));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_rust_compiler_errors() {
        assert_eq!(
            classify_failure("error[E0433]: failed to resolve: use of undeclared crate or module"),
            FailureCategory::CompileError
        );
        assert_eq!(
            classify_failure("syntax error: unexpected token"),
            FailureCategory::CompileError
        );
    }

    #[test]
    fn classifies_gate_and_score_failures() {
        assert_eq!(
            classify_failure("required check failed: cargo test --test gate_auth"),
            FailureCategory::GateCheckFailed
        );
    }

    #[test]
    fn classifies_plateau_and_timeouts() {
        assert_eq!(
            classify_failure("stopped: NoProgress across 3 rounds"),
            FailureCategory::ScorePlateau
        );
        assert_eq!(
            classify_failure("exceeded token budget: OverBudget"),
            FailureCategory::TimeoutOrExhausted
        );
    }

    #[test]
    fn computes_effectiveness_and_reuse_percentages() {
        let runs = vec![
            GoalRunMetric {
                goal_id: "goal-1".into(),
                title: "Inspection Domain".into(),
                accepted: true,
                attempts: 2,
                score: 1000,
                spent_tokens: 5000,
                stop_reason: "Accepted".into(),
                failures: vec![],
                components_reused: 4,
                components_written: 1,
                pr_opened: true,
                pr_merged: true,
            },
            GoalRunMetric {
                goal_id: "goal-2".into(),
                title: "Marketplace Domain".into(),
                accepted: false,
                attempts: 4,
                score: 650,
                spent_tokens: 12000,
                stop_reason: "NoProgress".into(),
                failures: vec![
                    "error[E0308]: mismatched types".into(),
                    "gate check failed: test_checkout".into(),
                ],
                components_reused: 6,
                components_written: 1,
                pr_opened: false,
                pr_merged: false,
            },
        ];

        let summary = EffectivenessSummary::compute(&runs);
        assert_eq!(summary.total_goals, 2);
        assert_eq!(summary.accepted_goals, 1);
        assert_eq!(summary.yield_percentage, 50.0);
        assert_eq!(summary.prs_opened, 1);
        assert_eq!(summary.prs_merged, 1);
        assert_eq!(summary.pr_success_rate, 100.0);
        assert_eq!(summary.components_reused, 10);
        assert_eq!(summary.components_written, 2);
        // 10 / 12 = 83.333%
        assert!((summary.component_reuse_ratio - 83.33).abs() < 0.1);
        assert_eq!(summary.failure_distribution.get("CompileError"), Some(&1));
        assert_eq!(summary.failure_distribution.get("GateCheckFailed"), Some(&1));
    }
}
