//! Request bodies, as types.
//!
//! The rest of the platform refuses a misspelled key and says which are legal (`comp.toml`, `AppSpec`,
//! component config keys). The HTTP API does too: otherwise `POST /api/deployments` with `"noodes"` would
//! create an empty-graph deployment and report success.
//!
//! `deny_unknown_fields` plus serde's own message is the whole mechanism: `unknown field 'noodes', expected one
//! of 'name', 'strategy', 'nodes', 'edges'` is the shape ADR-0010 asked for.

use serde::Deserialize;
use serde_json::Value;

use crate::Outcome;

/// Parse a body into `T`, or produce the 422 the author needs. 422 rather than 400: the JSON was fine,
/// it said something this endpoint does not accept, and a 400 would suggest a broken serialiser.
pub fn parse<T: for<'de> Deserialize<'de>>(raw: &[u8]) -> Result<T, Outcome> {
    // An empty body is an empty object, so endpoints whose fields are all optional keep working without one.
    if raw.is_empty() {
        return serde_json::from_str::<T>("{}")
            .map_err(|e| Outcome::Err(422, format!("empty body: {e}")));
    }
    serde_json::from_slice::<T>(raw).map_err(|e| Outcome::Err(422, e.to_string()))
}

/// `POST /api/deployments`
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateDeployment {
    #[serde(default)]
    pub name: String,
    /// `fused` or `linked`; validated by `Strategy::parse`, not here, so the error names the legal values.
    #[serde(default)]
    pub strategy: Option<String>,
    /// The canvas: component ids, optionally with `config`.
    #[serde(default)]
    pub nodes: Vec<Value>,
    #[serde(default)]
    pub edges: Vec<Value>,
}

/// `POST /api/deployments/{id}/save` — every field optional, because a save may
/// change the graph or just re-save what is there.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveDeployment {
    #[serde(default)]
    pub nodes: Option<Vec<Value>>,
    #[serde(default)]
    pub edges: Option<Vec<Value>>,
    #[serde(default)]
    pub strategy: Option<String>,
}

/// `POST /api/secrets`
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutSecret {
    pub name: String,
    /// The plaintext, on its way to the vault and nowhere else.
    pub value: String,
}

/// `POST /api/components/satisfies`
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Satisfies {
    pub socket: String,
    pub plug: String,
}

/// The config a deployment's node carries, by component id. Pulled out of `deployment_save` so its test
/// has a real function to reference.
pub fn node_config(
    nodes: &[serde_json::Value],
    id: &str,
) -> serde_json::Map<String, serde_json::Value> {
    nodes
        .iter()
        .find(|n| n["id"].as_str() == Some(id))
        .and_then(|n| n["config"].as_object())
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Outcome` carries a response, not a diagnostic, so it has no `Debug` and `unwrap` cannot report why.
    fn ok<T>(r: Result<T, Outcome>) -> T {
        match r {
            Ok(v) => v,
            Err(Outcome::Err(code, msg)) => panic!("expected success, got {code}: {msg}"),
            Err(_) => panic!("expected success"),
        }
    }

    fn err<T: for<'de> Deserialize<'de>>(body: &str) -> String {
        match parse::<T>(body.as_bytes()) {
            Err(Outcome::Err(code, msg)) => format!("{code} {msg}"),
            _ => panic!("expected a refusal for {body}"),
        }
    }

    #[test]
    fn a_misspelled_field_is_named_along_with_the_legal_ones() {
        // The bug this file exists for: `noodes` created an empty-graph deployment and reported success.
        let e = err::<CreateDeployment>(r#"{"name":"shop","noodes":[]}"#);
        assert!(e.starts_with("422"), "{e}");
        assert!(e.contains("noodes"), "the typo must be named: {e}");
        assert!(e.contains("nodes"), "the legal fields must be offered: {e}");
    }

    #[test]
    fn a_misspelled_strategy_field_no_longer_silently_fuses() {
        let e = err::<CreateDeployment>(r#"{"name":"shop","stratgey":"linked"}"#);
        assert!(e.contains("stratgey") && e.contains("strategy"), "{e}");
    }

    #[test]
    fn a_valid_body_parses_with_defaults_for_what_is_absent() {
        let c: CreateDeployment = ok(parse(br#"{"name":"shop"}"#));
        assert_eq!(c.name, "shop");
        assert!(c.nodes.is_empty() && c.edges.is_empty());
        assert_eq!(c.strategy, None, "absent, not defaulted here — the handler decides");
    }

    #[test]
    fn an_empty_body_is_an_empty_save_rather_than_an_error() {
        // `POST /save` with no body means "save what is already there", which several callers rely on.
        let s: SaveDeployment = ok(parse(b""));
        assert!(s.nodes.is_none() && s.edges.is_none() && s.strategy.is_none());
    }

    #[test]
    fn a_missing_required_field_is_refused_and_named() {
        // `satisfies` needs both sides; there is no sensible default for either.
        let e = err::<Satisfies>(r#"{"socket":"gate"}"#);
        assert!(e.contains("plug"), "{e}");
    }

    #[test]
    fn config_is_read_off_the_node_it_belongs_to() {
        let nodes: Vec<Value> =
            serde_json::from_str(r#"[{"id":"gate","config":{"token":"abc"}},{"id":"store"}]"#)
                .unwrap();
        assert_eq!(node_config(&nodes, "gate").get("token"), Some(&Value::from("abc")));
        assert!(node_config(&nodes, "store").is_empty(), "no config is not an error");
        assert!(node_config(&nodes, "ghost").is_empty());
    }
}

/// `POST /api/projects`
///
/// One repository per project (ADR-0082). Multi-repo is deliberately not built: everything saying "the
/// base" would have to become "the base of each".
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewProject {
    pub name: String,
    /// `owner/name` on the forge.
    pub repo: String,
    #[serde(default)]
    pub base: Option<String>,
    /// A `vault://` reference. Never a token: a manifest and a record are the same kind of place (ADR-0010).
    #[serde(default)]
    pub forge_token_ref: Option<String>,
    #[serde(default)]
    pub llm_key_ref: Option<String>,
    /// Units per run. Recorded, and enforced by nothing yet (ADR-0082).
    #[serde(default)]
    pub budget: Option<u64>,
}

/// `POST /api/projects/{project}/goals`
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewGoal {
    pub title: String,
    /// A path in the repository — `.comp/goals/x.md`. The spec lives in git so it is versioned and reviewable (ADR-0081).
    #[serde(default)]
    pub spec: Option<String>,
    /// Lower runs sooner. Only an ordering hint: a human starts every goal.
    #[serde(default)]
    pub priority: Option<i64>,
    /// The goal this one is a part of, when it is a sub-goal. Otherwise an ORDINARY goal (same lifecycle,
    /// queue and "a human starts it" rule, ADR-0082); this lets a decomposition outlive one run.
    ///
    /// The knowledge pool holds the same relationship as a `decomposes_into` edge keyed by goal TEXT; this is
    /// keyed by goal ID: the pool says what has been learned, the queue what is still to do.
    #[serde(default)]
    pub parent: Option<String>,
}

/// `POST /api/goals/{id}/fail`
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailGoal {
    /// Why. A dead-letter entry with no reason cannot be acted on.
    pub reason: String,
}

/// `POST /api/goals/{id}/review`
///
/// Optional and additive: without a PR to name, the goal still transitions but carries no link, so
/// nothing records which pull request an `awaiting-human` goal waits on.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewGoal {
    #[serde(default)]
    pub pr: Option<String>,
}

/// `POST /api/projects/{project}/events/ack`
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AckEvents {
    pub group: String,
    pub ids: Vec<String>,
}
