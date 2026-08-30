//! Request bodies, as types.
//!
//! Everything else in this platform now refuses a misspelled key and says which ones
//! are legal: `comp.toml` (ADR-0041's settings), the app spec (`AppSpec`), and a
//! component's config keys (ADR-0047). The HTTP API was the exception — handlers read
//! `b["name"]` and an unknown field simply did nothing.
//!
//! That is the same bug in a different place. `POST /api/deployments` with `"noodes"`
//! created a deployment with an empty graph and reported success; `"stratgey"` got
//! you a fused build you did not ask for. The author is told nothing, and finds out
//! at deploy or in production.
//!
//! `deny_unknown_fields` plus serde's own message is the whole fix — it already
//! produces `unknown field 'noodes', expected one of 'name', 'strategy', 'nodes',
//! 'edges'`, which is exactly the shape ADR-0010 asked for and ADR-0047 built by hand.

use serde::Deserialize;
use serde_json::Value;

use crate::Outcome;

/// Parse a body into `T`, or produce the 422 the author needs.
///
/// 422 rather than 400: the JSON was fine, it just said something this endpoint does
/// not accept. A 400 tells a client its serialiser is broken, which is misleading
/// when the real problem is a typo.
pub fn parse<T: for<'de> Deserialize<'de>>(raw: &[u8]) -> Result<T, Outcome> {
    // An empty body is an empty object, so endpoints whose fields are all optional
    // (a save that changes nothing) keep working without a body.
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
    /// `fused` or `linked`; validated by `Strategy::parse`, not here, so the error
    /// keeps naming the legal values.
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

/// The config a deployment's node carries, by component id.
///
/// Pulled out of `deployment_save` because its test was written here and the
/// function never was — so the test referenced a name that did not exist and the
/// whole native test target of this component failed to compile. Nothing in it
/// had run since, including anything added later.
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

    /// `Outcome` carries a response, not a diagnostic, so it has no `Debug` — which
    /// means `unwrap` cannot report why. Unwrapping by hand keeps the failure legible.
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
        // The bug this file exists for: `noodes` used to create a deployment with an
        // empty graph and report success.
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
        // `POST /save` with no body means "save what is already there", which several
        // callers rely on.
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
/// One repository per project (ADR-0082). Multi-repo is an open goal, and the
/// shape it would take — a list — is deliberately not built, because every
/// downstream thing that says "the base" would have to become "the base of each".
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewProject {
    pub name: String,
    /// `owner/name` on the forge.
    pub repo: String,
    #[serde(default)]
    pub base: Option<String>,
    /// A `vault://` reference. Never a token — a manifest and a record are the
    /// same kind of place as far as ADR-0010 is concerned.
    #[serde(default)]
    pub forge_token_ref: Option<String>,
    #[serde(default)]
    pub llm_key_ref: Option<String>,
    /// Units per run. Recorded, and enforced by nothing yet — said out loud in
    /// ADR-0082 so it cannot be mistaken for working.
    #[serde(default)]
    pub budget: Option<u64>,
}

/// `POST /api/projects/{project}/goals`
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewGoal {
    pub title: String,
    /// A path in the repository — `.comp/goals/x.md`. The spec lives in git so it
    /// is versioned, reviewable, and content-addressed for free (ADR-0081).
    #[serde(default)]
    pub spec: Option<String>,
    /// Lower runs sooner. Only an ordering hint: a human starts every goal, so
    /// this sorts a worklist rather than driving anything.
    #[serde(default)]
    pub priority: Option<i64>,
    /// The goal this one is a part of, when it is a sub-goal.
    ///
    /// A sub-goal is an ORDINARY goal in every other respect — same lifecycle,
    /// same queue, same "a human starts it" rule (ADR-0082). This field is the
    /// only difference, and it exists so a decomposition can be picked up later
    /// rather than living for the length of one run.
    ///
    /// The knowledge pool holds the same relationship as a `decomposes_into`
    /// edge, keyed by goal TEXT. This is keyed by goal ID and answers a different
    /// question: the pool says what has been learned, the queue says what is
    /// still to do.
    #[serde(default)]
    pub parent: Option<String>,
}

/// `POST /api/goals/{id}/fail`
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailGoal {
    /// Why. A dead-letter entry with no reason is a dead-letter entry nobody can
    /// act on, which is the only thing a DLQ is for.
    pub reason: String,
}
