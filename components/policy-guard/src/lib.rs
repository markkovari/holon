//! `policy-guard` — reference implementation of `policy:guard`.
//!
//! Declarative attribute-based access control (ABAC) / row-level authz that
//! *complements* `auth-guard`'s RBAC. RBAC answers "does this principal's ROLE
//! grant action X on resource TYPE Y"; it cannot answer "does this principal
//! own THIS row". `policy:guard` makes those checks a declarative, reusable
//! rule set: principal attrs + resource attrs + an action go in, an allow/deny
//! decision (with the deciding rule) comes out.
//!
//! Semantics:
//!   * **Default-deny** — if no rule matches a request, the decision is DENY
//!     (`rule_id == ""`, reason "no matching rule (default deny)").
//!   * **Deny-overrides** — rules are evaluated in ascending `priority` (lower
//!     wins); within the same priority a matching DENY beats a matching ALLOW.
//!   * Both sides of every condition go through the same `resolve()`: a side
//!     starting with `principal.`/`resource.` is looked up in the matching attr
//!     map (absent -> ""), anything else is a literal. So the canonical
//!     ownership rule `{left:"resource.owner", op:eq, right:"principal.subject"}`
//!     compares the resource's owner attr to the principal's subject attr.
//!
//! State is just the rule set per domain, kept in `wasi:keyvalue` so rules are
//! updatable without a redeploy. Rules live at `pol_{sanitize(domain)}` as the
//! serde_json encoding of a `Vec<StoredRule>`.

#[allow(warnings)]
mod bindings;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use bindings::exports::policy::guard::guard::{
    Attr, Condition, Decision, Effect, Guest, Op, PolicyError, Rule,
};
use bindings::wasi::keyvalue::store as kv;

struct Component;

const BUCKET: &str = "default";

// ---- stored mirror types ------------------------------------------------
//
// The WIT-generated `Op`/`Effect` enums don't derive serde, so we mirror the
// `rule`/`condition`/`op`/`effect` shapes with our own serde-deriving structs
// and convert to/from the WIT types on the boundary.

#[derive(Serialize, Deserialize)]
struct StoredCond {
    left: String,
    op: StoredOp,
    right: String,
}

#[derive(Serialize, Deserialize)]
struct StoredRule {
    id: String,
    action: String,
    effect: StoredEffect,
    conditions: Vec<StoredCond>,
    priority: u32,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum StoredOp {
    Eq,
    Ne,
    InList,
    Lt,
    Gt,
    Has,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum StoredEffect {
    Allow,
    Deny,
}

impl From<Op> for StoredOp {
    fn from(o: Op) -> Self {
        match o {
            Op::Eq => StoredOp::Eq,
            Op::Ne => StoredOp::Ne,
            Op::InList => StoredOp::InList,
            Op::Lt => StoredOp::Lt,
            Op::Gt => StoredOp::Gt,
            Op::Has => StoredOp::Has,
        }
    }
}

impl From<StoredOp> for Op {
    fn from(o: StoredOp) -> Self {
        match o {
            StoredOp::Eq => Op::Eq,
            StoredOp::Ne => Op::Ne,
            StoredOp::InList => Op::InList,
            StoredOp::Lt => Op::Lt,
            StoredOp::Gt => Op::Gt,
            StoredOp::Has => Op::Has,
        }
    }
}

impl From<Effect> for StoredEffect {
    fn from(e: Effect) -> Self {
        match e {
            Effect::Allow => StoredEffect::Allow,
            Effect::Deny => StoredEffect::Deny,
        }
    }
}

impl From<StoredEffect> for Effect {
    fn from(e: StoredEffect) -> Self {
        match e {
            StoredEffect::Allow => Effect::Allow,
            StoredEffect::Deny => Effect::Deny,
        }
    }
}

impl From<&Condition> for StoredCond {
    fn from(c: &Condition) -> Self {
        StoredCond {
            left: c.left.clone(),
            op: c.op.into(),
            right: c.right.clone(),
        }
    }
}

impl From<&StoredCond> for Condition {
    fn from(c: &StoredCond) -> Self {
        Condition {
            left: c.left.clone(),
            op: c.op.into(),
            right: c.right.clone(),
        }
    }
}

impl From<&Rule> for StoredRule {
    fn from(r: &Rule) -> Self {
        StoredRule {
            id: r.id.clone(),
            action: r.action.clone(),
            effect: r.effect.into(),
            conditions: r.conditions.iter().map(StoredCond::from).collect(),
            priority: r.priority,
        }
    }
}

impl From<&StoredRule> for Rule {
    fn from(r: &StoredRule) -> Self {
        Rule {
            id: r.id.clone(),
            action: r.action.clone(),
            effect: r.effect.into(),
            conditions: r.conditions.iter().map(Condition::from).collect(),
            priority: r.priority,
        }
    }
}

// ---- key naming ---------------------------------------------------------

/// Sanitize one opaque segment to NATS-legal kv chars (same byte scheme as
/// config-store's `sanitize`).
fn sanitize(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for b in seg.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

/// Storage key for a domain's rule set: `pol_{sanitize(domain)}`.
fn pol_key(domain: &str) -> String {
    format!("pol_{}", sanitize(domain))
}

// ---- kv plumbing --------------------------------------------------------

fn open() -> Result<kv::Bucket, PolicyError> {
    kv::open(BUCKET).map_err(|e| PolicyError::BackendUnavailable(format!("open: {e:?}")))
}

/// Load + decode the rule set for `domain`, returning an empty vec if absent.
fn load_rules(bucket: &kv::Bucket, domain: &str) -> Result<Vec<StoredRule>, PolicyError> {
    match bucket.get(&pol_key(domain)) {
        Ok(Some(bytes)) => serde_json::from_slice::<Vec<StoredRule>>(&bytes)
            .map_err(|e| PolicyError::BackendUnavailable(format!("corrupt rule set: {e}"))),
        Ok(None) => Ok(Vec::new()),
        Err(e) => Err(PolicyError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

// ---- validation ---------------------------------------------------------

/// Is this operand a reference (resolved from an attr map) rather than a
/// literal? References start with `principal.` or `resource.`.
fn is_reference(s: &str) -> bool {
    s.starts_with("principal.") || s.starts_with("resource.")
}

/// Validate one condition: both sides non-empty; if a side *looks* like a
/// reference prefix it must be a well-formed `principal.<key>`/`resource.<key>`
/// (a non-empty key after the dot). Anything not matching a reference prefix is
/// a literal and always allowed.
fn validate_cond(rule_id: &str, c: &Condition) -> Result<(), PolicyError> {
    let check_side = |label: &str, side: &str| -> Result<(), PolicyError> {
        if side.is_empty() {
            return Err(PolicyError::InvalidRule(format!(
                "rule {rule_id:?}: {label} operand is empty"
            )));
        }
        // A reference must have a non-empty key after the prefix dot.
        if is_reference(side) {
            let key = side.splitn(2, '.').nth(1).unwrap_or("");
            if key.is_empty() {
                return Err(PolicyError::InvalidRule(format!(
                    "rule {rule_id:?}: {label} reference {side:?} has empty key"
                )));
            }
        }
        Ok(())
    };
    check_side("left", &c.left)?;
    check_side("right", &c.right)?;
    Ok(())
}

// ---- evaluation ---------------------------------------------------------

/// Resolve one operand: a `principal.`/`resource.` reference is looked up in
/// the matching attr map (absent -> ""); anything else is the literal itself.
fn resolve(side: &str, principal: &HashMap<String, String>, resource: &HashMap<String, String>) -> String {
    if let Some(key) = side.strip_prefix("principal.") {
        principal.get(key).cloned().unwrap_or_default()
    } else if let Some(key) = side.strip_prefix("resource.") {
        resource.get(key).cloned().unwrap_or_default()
    } else {
        side.to_string()
    }
}

/// Does a single condition hold for the given attr maps?
fn cond_holds(
    c: &StoredCond,
    principal: &HashMap<String, String>,
    resource: &HashMap<String, String>,
) -> bool {
    let left = resolve(&c.left, principal, resource);
    let right = resolve(&c.right, principal, resource);
    match c.op {
        StoredOp::Eq => left == right,
        StoredOp::Ne => left != right,
        StoredOp::InList => right.split(',').any(|item| item.trim() == left),
        StoredOp::Lt => match (left.parse::<f64>(), right.parse::<f64>()) {
            (Ok(l), Ok(r)) => l < r,
            _ => false,
        },
        StoredOp::Gt => match (left.parse::<f64>(), right.parse::<f64>()) {
            (Ok(l), Ok(r)) => l > r,
            _ => false,
        },
        StoredOp::Has => left.split(',').any(|item| item.trim() == right),
    }
}

/// Build a `key -> value` lookup map from an attr list.
fn attr_map(attrs: &[Attr]) -> HashMap<String, String> {
    attrs.iter().map(|a| (a.key.clone(), a.value.clone())).collect()
}

impl Guest for Component {
    fn set_rules(domain: String, rules: Vec<Rule>) -> Result<(), PolicyError> {
        // Validate every condition of every rule before persisting anything.
        for r in &rules {
            for c in &r.conditions {
                validate_cond(&r.id, c)?;
            }
        }
        let stored: Vec<StoredRule> = rules.iter().map(StoredRule::from).collect();
        let body = serde_json::to_vec(&stored)
            .map_err(|e| PolicyError::BackendUnavailable(format!("encode: {e}")))?;
        let bucket = open()?;
        bucket
            .set(&pol_key(&domain), &body)
            .map_err(|e| PolicyError::BackendUnavailable(format!("set: {e:?}")))?;
        Ok(())
    }

    fn get_rules(domain: String) -> Result<Vec<Rule>, PolicyError> {
        let bucket = open()?;
        let stored = load_rules(&bucket, &domain)?;
        Ok(stored.iter().map(Rule::from).collect())
    }

    fn can(
        domain: String,
        action: String,
        principal: Vec<Attr>,
        target_attrs: Vec<Attr>,
    ) -> Result<Decision, PolicyError> {
        let bucket = open()?;
        let mut rules = load_rules(&bucket, &domain)?;
        let pmap = attr_map(&principal);
        let rmap = attr_map(&target_attrs);

        // Consider only rules whose action matches (exact or wildcard).
        rules.retain(|r| r.action == action || r.action == "*");
        // Stable sort by ascending priority — lower priority wins. Within the
        // same priority a DENY must be seen before an ALLOW so deny overrides.
        rules.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then(deny_first(a.effect).cmp(&deny_first(b.effect)))
        });

        for r in &rules {
            if r.conditions.iter().all(|c| cond_holds(c, &pmap, &rmap)) {
                let allowed = matches!(r.effect, StoredEffect::Allow);
                let reason = if allowed {
                    format!("allowed by rule {:?}", r.id)
                } else {
                    format!("denied by rule {:?}", r.id)
                };
                return Ok(Decision {
                    allowed,
                    rule_id: r.id.clone(),
                    reason,
                });
            }
        }

        // No rule matched -> default deny.
        Ok(Decision {
            allowed: false,
            rule_id: String::new(),
            reason: "no matching rule (default deny)".to_string(),
        })
    }

    fn enforce(
        domain: String,
        action: String,
        principal: Vec<Attr>,
        target_attrs: Vec<Attr>,
    ) -> bool {
        // Deny on error: any PolicyError collapses to `false` (fail closed).
        match Self::can(domain, action, principal, target_attrs) {
            Ok(d) => d.allowed,
            Err(_) => false,
        }
    }
}

/// Sort key making DENY sort before ALLOW at equal priority (deny-overrides).
fn deny_first(e: StoredEffect) -> u8 {
    match e {
        StoredEffect::Deny => 0,
        StoredEffect::Allow => 1,
    }
}

bindings::export!(Component with_types_in bindings);
