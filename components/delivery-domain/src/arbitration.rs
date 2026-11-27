//! Multi-rollout concurrency arbitration and collision detection.
//!
//! When multiple deliverables roll out concurrently within one project, they can
//! collide on the same WIT capability, pollute shared KPIs, or trigger cascading
//! guardrail rollbacks. Each project chooses a live, changeable `ReleaseStrategy`
//! stored in KV.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::sdlc::{Deliverable, Stage};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseStrategy {
    /// Strict queue per capability (oldest active wins; newer ones wait in `stg`).
    #[default]
    Fifo,
    /// Only deliverables explicitly approved in `picked_ids` may enter `ab_test`/`rollout`.
    HumanPick,
    /// Unrestricted parallel rollouts; emits warnings on same-capability collisions.
    FreeForAll,
    /// Concurrent rollouts receive disjoint subject hash slots (zero user overlap).
    MutuallyExclusiveLayers,
    /// Higher-priority deliverables (0 = P0 hotfix) preempt lower-priority rollouts.
    PriorityPreempt,
    /// Synchronized release train: members advance together and halt if any member rolls back.
    TrainBatch,
}

impl ReleaseStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            ReleaseStrategy::Fifo => "fifo",
            ReleaseStrategy::HumanPick => "human_pick",
            ReleaseStrategy::FreeForAll => "free_for_all",
            ReleaseStrategy::MutuallyExclusiveLayers => "mutually_exclusive_layers",
            ReleaseStrategy::PriorityPreempt => "priority_preempt",
            ReleaseStrategy::TrainBatch => "train_batch",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "fifo" => Some(ReleaseStrategy::Fifo),
            "human_pick" | "human-pick" | "manual" => Some(ReleaseStrategy::HumanPick),
            "free_for_all" | "free-for-all" | "parallel" => Some(ReleaseStrategy::FreeForAll),
            "mutually_exclusive_layers" | "exclusive_layers" | "layers" => {
                Some(ReleaseStrategy::MutuallyExclusiveLayers)
            }
            "priority_preempt" | "priority" | "preempt" => Some(ReleaseStrategy::PriorityPreempt),
            "train_batch" | "train" | "batch" => Some(ReleaseStrategy::TrainBatch),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectPolicy {
    pub project: String,
    #[serde(default)]
    pub strategy: ReleaseStrategy,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_rollouts: usize,
    /// Deliverable IDs explicitly picked by a human operator (used by `human_pick`).
    #[serde(default)]
    pub picked_ids: Vec<String>,
    /// Deliverable ID -> priority rank (0 = P0 hotfix, 1 = P1, 2 = P2 default).
    #[serde(default)]
    pub priorities: BTreeMap<String, u32>,
}

fn default_max_concurrent() -> usize {
    1
}

impl ProjectPolicy {
    pub fn default_for(project: &str) -> Self {
        Self {
            project: project.to_string(),
            strategy: ReleaseStrategy::Fifo,
            max_concurrent_rollouts: 1,
            picked_ids: Vec::new(),
            priorities: BTreeMap::new(),
        }
    }

    pub fn priority_of(&self, deliverable_id: &str) -> u32 {
        self.priorities.get(deliverable_id).copied().unwrap_or(2)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollisionReport {
    pub kind: String,
    pub capability: String,
    pub deliverable_ids: Vec<String>,
    pub severity: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldItem {
    pub deliverable_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArbitrationOutcome {
    pub strategy: ReleaseStrategy,
    pub allowed_ids: Vec<String>,
    pub held: Vec<HeldItem>,
    /// For `mutually_exclusive_layers`: deliverable_id -> (slot_index, total_slots).
    pub traffic_slices: BTreeMap<String, (u32, u32)>,
    pub collisions: Vec<CollisionReport>,
}

impl ArbitrationOutcome {
    pub fn hold_reason(&self, deliverable_id: &str) -> Option<&str> {
        self.held.iter().find(|h| h.deliverable_id == deliverable_id).map(|h| h.reason.as_str())
    }
}

/// Detect capability and KPI collisions among deliverables currently in active
/// post-staging stages (`ab_test` or `rollout`) or queued in `stg`.
pub fn detect_collisions(deliverables: &[Deliverable]) -> Vec<CollisionReport> {
    let mut by_cap: BTreeMap<&str, Vec<&Deliverable>> = BTreeMap::new();
    let mut active_prod: Vec<&Deliverable> = Vec::new();

    for d in deliverables {
        if matches!(d.stage, Stage::Stg | Stage::AbTest | Stage::Rollout) {
            by_cap.entry(d.capability.as_str()).or_default().push(d);
        }
        if matches!(d.stage, Stage::AbTest | Stage::Rollout) {
            active_prod.push(d);
        }
    }

    let mut collisions = Vec::new();
    for (cap, group) in by_cap {
        let prod_in_cap: Vec<&Deliverable> = group
            .iter()
            .copied()
            .filter(|d| matches!(d.stage, Stage::AbTest | Stage::Rollout))
            .collect();
        if prod_in_cap.len() > 1 {
            collisions.push(CollisionReport {
                kind: "same_capability_overlap".to_string(),
                capability: cap.to_string(),
                deliverable_ids: prod_in_cap.iter().map(|d| d.id.clone()).collect(),
                severity: "high".to_string(),
                message: format!(
                    "{} deliverables concurrently modifying `{cap}` in prod traffic",
                    prod_in_cap.len()
                ),
            });
        }
    }

    if active_prod.len() > 1 {
        let caps: std::collections::BTreeSet<&str> =
            active_prod.iter().map(|d| d.capability.as_str()).collect();
        if caps.len() > 1 {
            collisions.push(CollisionReport {
                kind: "shared_kpi_interaction".to_string(),
                capability: "*".to_string(),
                deliverable_ids: active_prod.iter().map(|d| d.id.clone()).collect(),
                severity: "medium".to_string(),
                message: format!(
                    "{} concurrent rollouts across different capabilities share project KPIs",
                    active_prod.len()
                ),
            });
        }
    }

    collisions
}

/// Evaluate which deliverables in a project are allowed to advance/serve live
/// traffic (`ab_test`/`rollout`) and which must hold according to `policy`.
pub fn arbitrate_project(
    policy: &ProjectPolicy,
    deliverables: &[Deliverable],
) -> ArbitrationOutcome {
    let collisions = detect_collisions(deliverables);
    let mut allowed_ids = Vec::new();
    let mut held = Vec::new();
    let mut traffic_slices = BTreeMap::new();

    let mut contenders: Vec<&Deliverable> = deliverables
        .iter()
        .filter(|d| matches!(d.stage, Stage::Stg | Stage::AbTest | Stage::Rollout))
        .collect();
    // Deterministic ordering: oldest first, then by id.
    contenders.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));

    match policy.strategy {
        ReleaseStrategy::FreeForAll => {
            for d in contenders {
                allowed_ids.push(d.id.clone());
            }
        }
        ReleaseStrategy::Fifo => {
            let cap_limit = policy.max_concurrent_rollouts.max(1);
            let mut per_cap_active: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
            for d in contenders {
                let active = per_cap_active.entry(d.capability.as_str()).or_default();
                if active.len() < cap_limit {
                    active.push(d.id.as_str());
                    allowed_ids.push(d.id.clone());
                } else {
                    let head = active[0];
                    held.push(HeldItem {
                        deliverable_id: d.id.clone(),
                        reason: format!(
                            "FIFO queue: waiting behind `{head}` on capability `{}`",
                            d.capability
                        ),
                    });
                }
            }
        }
        ReleaseStrategy::HumanPick => {
            for d in contenders {
                if policy.picked_ids.iter().any(|id| id == &d.id) {
                    allowed_ids.push(d.id.clone());
                } else {
                    held.push(HeldItem {
                        deliverable_id: d.id.clone(),
                        reason: "human_pick: awaiting explicit operator selection".to_string(),
                    });
                }
            }
        }
        ReleaseStrategy::MutuallyExclusiveLayers => {
            let total = contenders.len() as u32;
            for (idx, d) in contenders.into_iter().enumerate() {
                allowed_ids.push(d.id.clone());
                if total > 0 {
                    traffic_slices.insert(d.id.clone(), (idx as u32, total));
                }
            }
        }
        ReleaseStrategy::PriorityPreempt => {
            let mut by_cap: BTreeMap<&str, Vec<&Deliverable>> = BTreeMap::new();
            for d in contenders {
                by_cap.entry(d.capability.as_str()).or_default().push(d);
            }
            for (cap, mut group) in by_cap {
                group.sort_by(|a, b| {
                    policy
                        .priority_of(&a.id)
                        .cmp(&policy.priority_of(&b.id))
                        .then_with(|| a.created_at.cmp(&b.created_at))
                });
                if let Some((winner, rest)) = group.split_first() {
                    let win_prio = policy.priority_of(&winner.id);
                    allowed_ids.push(winner.id.clone());
                    for loser in rest {
                        let lose_prio = policy.priority_of(&loser.id);
                        held.push(HeldItem {
                            deliverable_id: loser.id.clone(),
                            reason: format!(
                                "priority_preempt: P{lose_prio} paused by P{win_prio} `{}` on `{cap}`",
                                winner.id
                            ),
                        });
                    }
                }
            }
        }
        ReleaseStrategy::TrainBatch => {
            let rolled_back_member = deliverables.iter().find(|d| d.stage == Stage::RolledBack);
            if let Some(bad) = rolled_back_member {
                for d in contenders {
                    held.push(HeldItem {
                        deliverable_id: d.id.clone(),
                        reason: format!("train_batch: train halted after `{}` rolled back", bad.id),
                    });
                }
            } else {
                for d in contenders {
                    allowed_ids.push(d.id.clone());
                }
            }
        }
    }

    ArbitrationOutcome { strategy: policy.strategy, allowed_ids, held, traffic_slices, collisions }
}

/// Deterministic FNV-1a bucket check for `mutually_exclusive_layers`.
pub fn subject_in_slice(subject: &str, slot: u32, total: u32) -> bool {
    if total <= 1 {
        return true;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in subject.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (h % u64::from(total)) as u32 == slot
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdlc::{Guardrails, VariantSpec, WiringPolicy};

    fn mk(id: &str, cap: &str, stage: Stage, created_at: u64) -> Deliverable {
        Deliverable {
            id: id.into(),
            project: "pet-clinic".into(),
            name: id.into(),
            capability: cap.into(),
            goal_ref: String::new(),
            stage,
            control: VariantSpec {
                id: "v1".into(),
                role: "control".into(),
                language: "moonbit".into(),
                digest: String::new(),
                weight: 50,
                endpoint: None,
                config: BTreeMap::new(),
            },
            candidate: VariantSpec {
                id: "v2".into(),
                role: "candidate".into(),
                language: "moonbit".into(),
                digest: String::new(),
                weight: 50,
                endpoint: None,
                config: BTreeMap::new(),
            },
            wiring: WiringPolicy::default(),
            guardrails: Guardrails::default(),
            created_at,
            updated_at: created_at,
        }
    }

    #[test]
    fn fifo_queues_second_deliverable_on_same_capability() {
        let policy = ProjectPolicy::default_for("pet-clinic");
        let ds = vec![
            mk("d1", "clinic:triage@0.1.0", Stage::Rollout, 100),
            mk("d2", "clinic:triage@0.1.0", Stage::Stg, 200),
            mk("d3", "clinic:billing@0.1.0", Stage::AbTest, 150),
        ];
        let out = arbitrate_project(&policy, &ds);
        assert_eq!(out.allowed_ids, vec!["d1", "d3"]);
        assert!(out.hold_reason("d2").unwrap().contains("FIFO queue"));
    }

    #[test]
    fn human_pick_holds_unselected_deliverables() {
        let mut policy = ProjectPolicy::default_for("pet-clinic");
        policy.strategy = ReleaseStrategy::HumanPick;
        policy.picked_ids = vec!["d2".into()];
        let ds = vec![
            mk("d1", "clinic:triage@0.1.0", Stage::Stg, 100),
            mk("d2", "clinic:triage@0.1.0", Stage::Stg, 200),
        ];
        let out = arbitrate_project(&policy, &ds);
        assert_eq!(out.allowed_ids, vec!["d2"]);
        assert!(out.hold_reason("d1").unwrap().contains("human_pick"));
    }

    #[test]
    fn priority_preempt_lets_p0_hotfix_pause_p2_feature() {
        let mut policy = ProjectPolicy::default_for("pet-clinic");
        policy.strategy = ReleaseStrategy::PriorityPreempt;
        policy.priorities.insert("hotfix".into(), 0);
        policy.priorities.insert("feature".into(), 2);
        let ds = vec![
            mk("feature", "clinic:triage@0.1.0", Stage::Rollout, 100),
            mk("hotfix", "clinic:triage@0.1.0", Stage::Stg, 200),
        ];
        let out = arbitrate_project(&policy, &ds);
        assert_eq!(out.allowed_ids, vec!["hotfix"]);
        assert!(out.hold_reason("feature").unwrap().contains("P0"));
    }

    #[test]
    fn train_batch_halts_all_members_when_one_rolls_back() {
        let mut policy = ProjectPolicy::default_for("pet-clinic");
        policy.strategy = ReleaseStrategy::TrainBatch;
        let ds = vec![
            mk("d1", "clinic:triage@0.1.0", Stage::RolledBack, 100),
            mk("d2", "clinic:billing@0.1.0", Stage::Rollout, 200),
        ];
        let out = arbitrate_project(&policy, &ds);
        assert!(out.allowed_ids.is_empty());
        assert!(out.hold_reason("d2").unwrap().contains("train halted"));
    }

    #[test]
    fn mutually_exclusive_layers_assigns_disjoint_subject_slices() {
        let mut policy = ProjectPolicy::default_for("pet-clinic");
        policy.strategy = ReleaseStrategy::MutuallyExclusiveLayers;
        let ds = vec![
            mk("d1", "clinic:triage@0.1.0", Stage::AbTest, 100),
            mk("d2", "clinic:billing@0.1.0", Stage::AbTest, 200),
        ];
        let out = arbitrate_project(&policy, &ds);
        assert_eq!(out.traffic_slices.get("d1"), Some(&(0, 2)));
        assert_eq!(out.traffic_slices.get("d2"), Some(&(1, 2)));
        // Every subject lands in exactly one slice.
        for i in 0..20 {
            let s = format!("owner-{i}");
            assert_ne!(subject_in_slice(&s, 0, 2), subject_in_slice(&s, 1, 2));
        }
    }
}
