//! Pure SDLC state machine, context-based wiring resolver, post-release judge,
//! and multi-project aggregation.
//!
//! Kept free of WIT host calls so every transition, routing precedence rule,
//! lift calculation, inline guardrail rollback, and aggregation is unit-tested natively.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Draft,
    Gated,
    Stg,
    AbTest,
    Rollout,
    Promoted,
    RolledBack,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Draft => "draft",
            Stage::Gated => "gated",
            Stage::Stg => "stg",
            Stage::AbTest => "ab_test",
            Stage::Rollout => "rollout",
            Stage::Promoted => "promoted",
            Stage::RolledBack => "rolled_back",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(Stage::Draft),
            "gated" => Some(Stage::Gated),
            "stg" | "staging" => Some(Stage::Stg),
            "ab_test" | "ab-test" | "experiment" => Some(Stage::AbTest),
            "rollout" | "gradual_rollout" => Some(Stage::Rollout),
            "promoted" | "prod" => Some(Stage::Promoted),
            "rolled_back" | "rollback" => Some(Stage::RolledBack),
            _ => None,
        }
    }

    pub fn can_transition_to(self, next: Stage) -> bool {
        if self == next {
            return true;
        }
        matches!(
            (self, next),
            (Stage::Draft, Stage::Gated)
                | (Stage::Gated, Stage::Stg)
                | (
                    Stage::Stg,
                    Stage::AbTest | Stage::Rollout | Stage::Promoted | Stage::RolledBack
                )
                | (Stage::AbTest, Stage::Rollout | Stage::Promoted | Stage::RolledBack)
                | (Stage::Rollout, Stage::Promoted | Stage::RolledBack)
                | (Stage::Promoted, Stage::RolledBack)
                | (Stage::RolledBack, Stage::Stg | Stage::Gated)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VariantSpec {
    pub id: String,
    /// "control" | "candidate"
    pub role: String,
    #[serde(default = "default_lang")]
    pub language: String,
    #[serde(default)]
    pub digest: String,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Arbitrary app-defined variant metadata (opaque to the SDLC substrate).
    #[serde(default)]
    pub config: BTreeMap<String, String>,
}

fn default_lang() -> String {
    "moonbit".to_string()
}

fn default_weight() -> u32 {
    50
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WiringPolicy {
    #[serde(default)]
    pub stg_tenants: Vec<String>,
    #[serde(default)]
    pub stg_cohorts: Vec<String>,
    #[serde(default = "default_true")]
    pub allow_stage_header: bool,
    #[serde(default = "default_steps")]
    pub rollout_steps: Vec<u8>,
    #[serde(default)]
    pub current_step: usize,
    #[serde(default)]
    pub rollout_pct: u8,
}

impl WiringPolicy {
    /// Align `current_step` to match `rollout_pct` against `rollout_steps` so
    /// creating or updating a deliverable at e.g. 50% doesn't leave `current_step` at 0.
    pub fn align_step(&mut self) {
        if self.rollout_steps.is_empty() {
            self.current_step = 0;
            return;
        }
        if let Some(pos) = self.rollout_steps.iter().position(|&p| p >= self.rollout_pct) {
            self.current_step = pos;
        } else {
            self.current_step = self.rollout_steps.len().saturating_sub(1);
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_steps() -> Vec<u8> {
    vec![10, 25, 50, 100]
}

impl Default for WiringPolicy {
    fn default() -> Self {
        Self {
            stg_tenants: Vec::new(),
            stg_cohorts: vec!["internal".to_string(), "beta-vets".to_string()],
            allow_stage_header: true,
            rollout_steps: default_steps(),
            current_step: 0,
            rollout_pct: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Guardrails {
    #[serde(default = "default_max_err")]
    pub max_error_rate: f64,
    #[serde(default = "default_min_exposures")]
    pub min_exposures: u64,
    #[serde(default)]
    pub min_lift: f64,
}

fn default_max_err() -> f64 {
    0.05
}

fn default_min_exposures() -> u64 {
    10
}

impl Default for Guardrails {
    fn default() -> Self {
        Self {
            max_error_rate: default_max_err(),
            min_exposures: default_min_exposures(),
            min_lift: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deliverable {
    pub id: String,
    pub project: String,
    pub name: String,
    pub capability: String,
    #[serde(default)]
    pub goal_ref: String,
    pub stage: Stage,
    pub control: VariantSpec,
    pub candidate: VariantSpec,
    #[serde(default)]
    pub wiring: WiringPolicy,
    #[serde(default)]
    pub guardrails: Guardrails,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserContext {
    #[serde(default)]
    pub tenant: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub cohort: String,
    #[serde(default)]
    pub stage: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WiringResolution {
    pub role: String,
    pub variant_id: String,
    pub digest: String,
    pub reason: String,
}

impl Deliverable {
    fn matches_stg_context(&self, ctx: &UserContext) -> Option<&'static str> {
        if self.wiring.allow_stage_header && ctx.stage.eq_ignore_ascii_case("stg") {
            return Some("stg_stage_header");
        }
        if !ctx.tenant.is_empty() && self.wiring.stg_tenants.iter().any(|t| t == &ctx.tenant) {
            return Some("stg_tenant_override");
        }
        if !ctx.cohort.is_empty() && self.wiring.stg_cohorts.iter().any(|c| c == &ctx.cohort) {
            return Some("stg_cohort_override");
        }
        None
    }

    fn pick(&self, use_candidate: bool, reason: &str) -> WiringResolution {
        let v = if use_candidate { &self.candidate } else { &self.control };
        WiringResolution {
            role: v.role.clone(),
            variant_id: v.id.clone(),
            digest: v.digest.clone(),
            reason: reason.to_string(),
        }
    }

    pub fn resolve_wiring(
        &self,
        ctx: &UserContext,
        ab_arm: Option<&str>,
        flag_enabled: bool,
    ) -> WiringResolution {
        match self.stage {
            Stage::RolledBack => self.pick(false, "rolled_back"),
            Stage::Promoted => self.pick(true, "promoted"),
            Stage::Draft | Stage::Gated => self.pick(false, "pre_staging"),
            Stage::Stg => {
                if let Some(why) = self.matches_stg_context(ctx) {
                    self.pick(true, why)
                } else {
                    self.pick(false, "prod_control")
                }
            }
            Stage::AbTest => {
                if let Some(why) = self.matches_stg_context(ctx) {
                    return self.pick(true, why);
                }
                match ab_arm {
                    Some(arm) if arm == self.candidate.id || arm == "candidate" => {
                        self.pick(true, "ab_experiment")
                    }
                    _ => self.pick(false, "ab_experiment"),
                }
            }
            Stage::Rollout => {
                if let Some(why) = self.matches_stg_context(ctx) {
                    return self.pick(true, why);
                }
                if flag_enabled {
                    self.pick(true, "gradual_rollout")
                } else {
                    self.pick(false, "gradual_rollout")
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VariantMetrics {
    pub variant_id: String,
    pub role: String,
    pub exposures: u64,
    pub conversions: u64,
    pub errors: u64,
    pub conversion_rate: f64,
    pub error_rate: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeliverableAnalytics {
    pub deliverable_id: String,
    pub stage: Stage,
    pub control: VariantMetrics,
    pub candidate: VariantMetrics,
    pub conversion_lift: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum JudgeVerdict {
    Hold { reason: String },
    Rollback { reason: String },
    Advance { next_stage: Stage, next_rollout_pct: u8, next_step: usize, reason: String },
}

/// Fast inline circuit-breaker check invoked immediately when an error outcome is recorded.
/// Returns `Some(reason)` if the error guardrail is breached and the deliverable must
/// roll back right now without waiting for `/api/tick`.
pub fn check_inline_error_guardrail(d: &Deliverable, a: &DeliverableAnalytics) -> Option<String> {
    if !matches!(d.stage, Stage::Stg | Stage::AbTest | Stage::Rollout) {
        return None;
    }
    let min_err_samples = d.guardrails.min_exposures.clamp(1, 5);
    if a.candidate.exposures >= min_err_samples
        && a.candidate.error_rate > d.guardrails.max_error_rate
    {
        return Some(format!(
            "inline circuit-breaker: candidate error_rate {:.3} exceeded max_error_rate {:.3} over {} exposures",
            a.candidate.error_rate, d.guardrails.max_error_rate, a.candidate.exposures
        ));
    }
    None
}

/// Post-release SDLC gate: evaluates live production metrics against guardrails and KPIs.
pub fn judge_post_release(d: &Deliverable, a: &DeliverableAnalytics) -> JudgeVerdict {
    if !matches!(d.stage, Stage::Stg | Stage::AbTest | Stage::Rollout) {
        return JudgeVerdict::Hold {
            reason: format!("stage {} is not auto-judged", d.stage.as_str()),
        };
    }

    if let Some(reason) = check_inline_error_guardrail(d, a) {
        return JudgeVerdict::Rollback { reason };
    }

    match d.stage {
        Stage::Stg => JudgeVerdict::Hold {
            reason: "staging waits for explicit promotion to ab_test or rollout".to_string(),
        },
        Stage::AbTest => {
            if a.control.exposures < d.guardrails.min_exposures
                || a.candidate.exposures < d.guardrails.min_exposures
            {
                return JudgeVerdict::Hold {
                    reason: format!(
                        "collecting samples (control={}, candidate={}, min={})",
                        a.control.exposures, a.candidate.exposures, d.guardrails.min_exposures
                    ),
                };
            }
            if a.conversion_lift >= d.guardrails.min_lift {
                let first_pct = d.wiring.rollout_steps.first().copied().unwrap_or(25);
                JudgeVerdict::Advance {
                    next_stage: Stage::Rollout,
                    next_rollout_pct: first_pct,
                    next_step: 0,
                    reason: format!(
                        "A/B lift {:+.3} >= min_lift {:+.3}; advancing to rollout {}%",
                        a.conversion_lift, d.guardrails.min_lift, first_pct
                    ),
                }
            } else {
                JudgeVerdict::Rollback {
                    reason: format!(
                        "A/B lift {:+.3} below min_lift {:+.3} after sufficient samples",
                        a.conversion_lift, d.guardrails.min_lift
                    ),
                }
            }
        }
        Stage::Rollout => {
            if a.candidate.exposures < d.guardrails.min_exposures {
                return JudgeVerdict::Hold {
                    reason: format!(
                        "waiting for rollout exposures ({}/{})",
                        a.candidate.exposures, d.guardrails.min_exposures
                    ),
                };
            }
            let next_idx = d.wiring.current_step + 1;
            if let Some(&next_pct) = d.wiring.rollout_steps.get(next_idx) {
                if next_pct >= 100 {
                    JudgeVerdict::Advance {
                        next_stage: Stage::Promoted,
                        next_rollout_pct: 100,
                        next_step: next_idx,
                        reason: "rollout reached 100% within guardrails; promoting deliverable"
                            .to_string(),
                    }
                } else {
                    JudgeVerdict::Advance {
                        next_stage: Stage::Rollout,
                        next_rollout_pct: next_pct,
                        next_step: next_idx,
                        reason: format!("rollout healthy; stepping up to {}%", next_pct),
                    }
                }
            } else {
                JudgeVerdict::Advance {
                    next_stage: Stage::Promoted,
                    next_rollout_pct: 100,
                    next_step: d.wiring.current_step,
                    reason: "all rollout steps cleared; promoting deliverable".to_string(),
                }
            }
        }
        _ => JudgeVerdict::Hold { reason: "terminal".to_string() },
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeliverableDetail {
    pub deliverable: Deliverable,
    pub analytics: DeliverableAnalytics,
    pub judge: JudgeVerdict,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectAggregate {
    pub project: String,
    pub total_deliverables: usize,
    pub stage_counts: BTreeMap<String, usize>,
    pub total_exposures: u64,
    pub total_conversions: u64,
    pub total_errors: u64,
    pub avg_conversion_lift: f64,
    pub health: String,
    pub deliverables: Vec<DeliverableDetail>,
}

/// Group deliverables by project and compute portfolio-level KPIs + per-project details.
pub fn aggregate_projects(items: Vec<DeliverableDetail>) -> Vec<ProjectAggregate> {
    let mut grouped: BTreeMap<String, Vec<DeliverableDetail>> = BTreeMap::new();
    for item in items {
        grouped.entry(item.deliverable.project.clone()).or_default().push(item);
    }

    grouped
        .into_iter()
        .map(|(project, deliverables)| {
            let mut stage_counts: BTreeMap<String, usize> = BTreeMap::new();
            let mut total_exposures = 0u64;
            let mut total_conversions = 0u64;
            let mut total_errors = 0u64;
            let mut lift_sum = 0.0f64;
            let mut has_rollback = false;
            let mut has_active = false;

            for d in &deliverables {
                *stage_counts.entry(d.deliverable.stage.as_str().to_string()).or_insert(0) += 1;
                total_exposures += d.analytics.control.exposures + d.analytics.candidate.exposures;
                total_conversions +=
                    d.analytics.control.conversions + d.analytics.candidate.conversions;
                total_errors += d.analytics.control.errors + d.analytics.candidate.errors;
                lift_sum += d.analytics.conversion_lift;
                if d.deliverable.stage == Stage::RolledBack {
                    has_rollback = true;
                }
                if matches!(d.deliverable.stage, Stage::Stg | Stage::AbTest | Stage::Rollout) {
                    has_active = true;
                }
            }

            let n = deliverables.len();
            let avg_conversion_lift = if n == 0 { 0.0 } else { lift_sum / n as f64 };
            let health = if has_rollback {
                "degraded".to_string()
            } else if has_active {
                "delivering".to_string()
            } else {
                "healthy".to_string()
            };

            ProjectAggregate {
                project,
                total_deliverables: n,
                stage_counts,
                total_exposures,
                total_conversions,
                total_errors,
                avg_conversion_lift,
                health,
                deliverables,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_deliverable(stage: Stage) -> Deliverable {
        Deliverable {
            id: "deliv-1".into(),
            project: "pet-clinic".into(),
            name: "senior-triage-v2".into(),
            capability: "clinic:triage/evaluator@0.1.0".into(),
            goal_ref: "goal/pet-clinic-senior-triage".into(),
            stage,
            control: VariantSpec {
                id: "triage-v1-mbt".into(),
                role: "control".into(),
                language: "moonbit".into(),
                digest: "sha256:aaa".into(),
                weight: 50,
                endpoint: None,
                config: BTreeMap::new(),
            },
            candidate: VariantSpec {
                id: "triage-v2-senior-care".into(),
                role: "candidate".into(),
                language: "moonbit".into(),
                digest: "sha256:bbb".into(),
                weight: 50,
                endpoint: None,
                config: BTreeMap::new(),
            },
            wiring: WiringPolicy {
                stg_tenants: vec!["pilot-clinic".into()],
                stg_cohorts: vec!["beta-vets".into()],
                allow_stage_header: true,
                rollout_steps: vec![10, 50, 100],
                current_step: 0,
                rollout_pct: 10,
            },
            guardrails: Guardrails { max_error_rate: 0.05, min_exposures: 10, min_lift: 0.02 },
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn parallel_stg_routes_only_matching_context_to_candidate() {
        let d = sample_deliverable(Stage::Stg);
        let prod_ctx = UserContext {
            tenant: "north-clinic".into(),
            subject: "user-1".into(),
            cohort: "general".into(),
            stage: "prod".into(),
        };
        assert_eq!(d.resolve_wiring(&prod_ctx, None, false).reason, "prod_control");

        let stg_ctx = UserContext { stage: "stg".into(), ..prod_ctx.clone() };
        assert_eq!(d.resolve_wiring(&stg_ctx, None, false).reason, "stg_stage_header");

        let pilot_ctx = UserContext { tenant: "pilot-clinic".into(), ..prod_ctx };
        assert_eq!(d.resolve_wiring(&pilot_ctx, None, false).reason, "stg_tenant_override");
    }

    #[test]
    fn inline_error_guardrail_trips_immediately_without_waiting_for_tick() {
        let d = sample_deliverable(Stage::Rollout);
        let a = DeliverableAnalytics {
            deliverable_id: d.id.clone(),
            stage: d.stage,
            control: VariantMetrics {
                variant_id: d.control.id.clone(),
                role: "control".into(),
                exposures: 20,
                conversions: 8,
                errors: 0,
                conversion_rate: 0.40,
                error_rate: 0.0,
            },
            candidate: VariantMetrics {
                variant_id: d.candidate.id.clone(),
                role: "candidate".into(),
                exposures: 6,
                conversions: 2,
                errors: 1,
                conversion_rate: 0.33,
                error_rate: 0.167,
            },
            conversion_lift: -0.07,
        };
        assert!(check_inline_error_guardrail(&d, &a).is_some());
        assert!(matches!(judge_post_release(&d, &a), JudgeVerdict::Rollback { .. }));
    }

    #[test]
    fn align_step_matches_initial_rollout_pct() {
        let mut w = WiringPolicy {
            rollout_steps: vec![25, 50, 100],
            rollout_pct: 50,
            current_step: 0,
            ..Default::default()
        };
        w.align_step();
        assert_eq!(w.current_step, 1);
    }

    #[test]
    fn aggregate_projects_groups_multiple_projects_and_computes_health() {
        let d1 = sample_deliverable(Stage::Promoted);
        let mut d2 = sample_deliverable(Stage::RolledBack);
        d2.id = "deliv-2".into();
        d2.project = "checkout-engine".into();

        let make_detail = |d: Deliverable, exp: u64, err: u64, lift: f64| {
            let a = DeliverableAnalytics {
                deliverable_id: d.id.clone(),
                stage: d.stage,
                control: VariantMetrics {
                    variant_id: d.control.id.clone(),
                    role: "control".into(),
                    exposures: exp,
                    conversions: 2,
                    errors: 0,
                    conversion_rate: 0.2,
                    error_rate: 0.0,
                },
                candidate: VariantMetrics {
                    variant_id: d.candidate.id.clone(),
                    role: "candidate".into(),
                    exposures: exp,
                    conversions: 4,
                    errors: err,
                    conversion_rate: 0.4,
                    error_rate: if exp == 0 { 0.0 } else { err as f64 / exp as f64 },
                },
                conversion_lift: lift,
            };
            let judge = judge_post_release(&d, &a);
            DeliverableDetail { deliverable: d, analytics: a, judge }
        };

        let agg =
            aggregate_projects(vec![make_detail(d1, 10, 0, 0.2), make_detail(d2, 6, 2, -0.1)]);
        assert_eq!(agg.len(), 2);
        assert_eq!(agg[0].project, "checkout-engine");
        assert_eq!(agg[0].health, "degraded");
        assert_eq!(agg[0].total_errors, 2);
        assert_eq!(agg[1].project, "pet-clinic");
        assert_eq!(agg[1].health, "healthy");
        assert_eq!(agg[1].total_exposures, 20);
    }
}
