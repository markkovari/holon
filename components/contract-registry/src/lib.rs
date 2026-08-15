//! `contract-registry` — what two parts of a decomposed goal build against.
//!
//! ## The three rules, and where each one lives
//!
//! 1. **Nothing blocks.** `ask` records and returns; `pending` is read at a
//!    generation boundary. There is no call in this component that waits for
//!    another part, because two parts waiting on each other is the deadlock the
//!    design exists to make impossible (ADR-0086). The cost is honest: a needed
//!    change costs a generation.
//! 2. **An amendment is a promotion.** `answer(granted)` produces a PROPOSED
//!    version; `ratify` makes it canonical, and only for the part that owns it and
//!    only on a passing score. Same rule as `knowledge:memory`'s trusted pool
//!    (ADR-0084), because a part that could amend the shared contract at will
//!    could poison every sibling.
//! 3. **A version mismatch is a refusal**, not a warning. Two candidates built
//!    against different versions will compile, deploy, and fail on one field.
//!
//! ## Where the guards are
//!
//! In the statements, not after a read. `ratify` carries `WHERE owner = …`,
//! `answer` carries `WHERE answered = false`, and the version counter is
//! `n += 1 RETURN n` rather than a read-then-write — two parts resolving requests
//! at the same boundary is the normal case, and the read-modify-write version of
//! that was measured landing 7 of 60 concurrent writes (ADR-0084).

#[allow(warnings)]
mod bindings;
mod surql;

use bindings::exports::contract::registry::registry::{
    Contract, Guest, RegistryError, Request, Verdict,
};
use bindings::knowledge::graph::store as graph;

use serde_json::Value;

struct Component;

fn graph_err(e: graph::GraphError) -> RegistryError {
    match e {
        graph::GraphError::Rejected(m) => RegistryError::Rejected(m),
        graph::GraphError::Unavailable(m) => RegistryError::Unavailable(m),
        graph::GraphError::NotConfigured(m) => RegistryError::Unavailable(m),
    }
}

/// Send SurrealQL through the graph, which owns the connection, the credentials,
/// the namespace bootstrap and the conflict retry.
fn ask_db(statement: &str) -> Result<Vec<Value>, RegistryError> {
    let body = graph::query(statement).map_err(graph_err)?;
    surql::rows(&body).map_err(RegistryError::Rejected)
}

fn verdict_name(v: Verdict) -> &'static str {
    match v {
        Verdict::Granted => "granted",
        Verdict::Denied => "denied",
        Verdict::Counter => "counter",
    }
}

fn verdict_of(name: &str) -> Verdict {
    match name {
        "granted" => Verdict::Granted,
        "counter" => Verdict::Counter,
        // An unreadable verdict is a refusal, never a grant: a decoding gap must
        // not amend anything.
        _ => Verdict::Denied,
    }
}

/// A gate score that actually passed. The scale belongs to the fitness function,
/// so this asserts only the sign (ADR-0081).
fn passed(gate_score: i32) -> bool {
    gate_score > 0
}

fn contract_of(row: &Value) -> Contract {
    let (version, body, canonical, owner, from_request) = surql::contract_of(row);
    Contract { version, body, canonical, owner, from_request }
}

fn request_of(row: &Value) -> Request {
    Request {
        id: surql::id_of(row),
        from_part: row["from_part"].as_str().unwrap_or_default().to_string(),
        to_part: row["to_part"].as_str().unwrap_or_default().to_string(),
        subject: row["subject"].as_str().unwrap_or_default().to_string(),
        body: row["body"].as_str().unwrap_or_default().to_string(),
        at_version: row["at_version"].as_u64().unwrap_or(0) as u32,
        answered: row["answered"].as_bool().unwrap_or(false),
        verdict: verdict_of(row["verdict"].as_str().unwrap_or("")),
        answer: row["answer"].as_str().unwrap_or_default().to_string(),
    }
}

fn next_version() -> Result<u32, RegistryError> {
    let rows = ask_db(&surql::next_version())?;
    rows.first()
        .and_then(|r| r["n"].as_u64())
        .map(|n| n as u32)
        .ok_or_else(|| RegistryError::Unavailable("the version counter answered nothing".into()))
}

impl Guest for Component {
    fn publish(body: String) -> Result<u32, RegistryError> {
        if body.trim().is_empty() {
            return Err(RegistryError::Refused(
                "a contract with no body is not something two parts can build against".into(),
            ));
        }
        // Refused rather than versioned: `publish` is the human's contract at the
        // start of a run, and a second one mid-run would silently move what every
        // part is building against without anybody granting anything.
        if !ask_db(&surql::current_contract())?.is_empty() {
            return Err(RegistryError::Refused(
                "a contract is already published — amend it through ask/answer, which leaves a \
                 record of who wanted what"
                    .into(),
            ));
        }
        let version = next_version()?;
        // Canonical on arrival, and owned by nobody: it is the human's, and there
        // is no part whose gate could ratify it.
        ask_db(&surql::put_contract(version, &body, true, "", ""))?;
        Ok(version)
    }

    fn current() -> Result<Contract, RegistryError> {
        let rows = ask_db(&surql::current_contract())?;
        rows.first().map(contract_of).ok_or_else(|| {
            RegistryError::Refused("no contract has been published for this goal".into())
        })
    }

    fn get(version: u32) -> Result<Option<Contract>, RegistryError> {
        Ok(ask_db(&surql::get_contract(version))?.first().map(contract_of))
    }

    fn proposed(part: String) -> Result<Option<Contract>, RegistryError> {
        Ok(ask_db(&surql::proposed_for(&part))?.first().map(contract_of))
    }

    fn ask(
        from_part: String,
        to_part: String,
        subject: String,
        body: String,
        at_version: u32,
    ) -> Result<String, RegistryError> {
        if from_part.trim().is_empty() || to_part.trim().is_empty() {
            return Err(RegistryError::Refused("a request needs both ends".into()));
        }
        if from_part == to_part {
            return Err(RegistryError::Refused(
                "a part asking itself for a change is a part editing its own contract".into(),
            ));
        }
        if subject.trim().is_empty() {
            return Err(RegistryError::Refused(
                "a request with no subject cannot be answered or deduplicated".into(),
            ));
        }
        let id = surql::request_id(&from_part, &to_part, &subject, at_version);
        ask_db(&surql::put_request(&id, &from_part, &to_part, &subject, &body, at_version))?;
        Ok(id)
    }

    fn pending(to_part: String) -> Result<Vec<Request>, RegistryError> {
        Ok(ask_db(&surql::pending(&to_part))?.iter().map(request_of).collect())
    }

    fn answer(id: String, v: Verdict, body: String) -> Result<u32, RegistryError> {
        let Some(row) = ask_db(&surql::get_request(&id))?.first().cloned() else {
            return Err(RegistryError::Refused(format!("no request {id}")));
        };
        let request = request_of(&row);
        if request.answered {
            return Err(RegistryError::Refused(format!(
                "request {id} was already answered ({})",
                verdict_name(request.verdict)
            )));
        }
        if body.trim().is_empty() {
            return Err(RegistryError::Refused(match v {
                Verdict::Granted => "a granted request needs the amended contract".into(),
                _ => "a refusal with no reason will be asked again next generation".to_string(),
            }));
        }

        // A grant is the only verdict that moves the contract, and what it produces
        // is PROPOSED: the part that granted it has not yet shown it can implement
        // what it agreed to.
        let version = match v {
            Verdict::Granted => {
                let version = next_version()?;
                ask_db(&surql::put_contract(version, &body, false, &request.to_part, &id))?;
                version
            }
            _ => 0,
        };
        // Guarded in the statement: two boundary passes resolving one request would
        // otherwise both succeed, and the second would overwrite the first verdict.
        if ask_db(&surql::answer(&id, verdict_name(v), &body, version))?.is_empty() {
            return Err(RegistryError::Refused(format!(
                "request {id} was answered by somebody else first"
            )));
        }
        Ok(version)
    }

    fn ratify(version: u32, part: String, gate_score: i32) -> Result<(), RegistryError> {
        if !passed(gate_score) {
            return Err(RegistryError::Refused(format!(
                "a gate score of {gate_score} did not pass — an amendment nobody can implement \
                 must not become what the other parts build against"
            )));
        }
        let Some(c) = Self::get(version)? else {
            return Err(RegistryError::Refused(format!("no contract v{version}")));
        };
        if c.canonical {
            return Ok(());
        }
        // `WHERE owner = …` in the statement, so a part cannot ratify a version it
        // does not own even if two ratifications race.
        if ask_db(&surql::ratify(version, &part))?.is_empty() {
            return Err(RegistryError::Refused(format!(
                "v{version} is owned by {:?}, not by {part:?} — the part that granted an \
                 amendment is the part that has to demonstrate it",
                c.owner
            )));
        }
        Ok(())
    }

    fn built_against(candidate: String, part: String, version: u32) -> Result<(), RegistryError> {
        if candidate.trim().is_empty() {
            return Err(RegistryError::Refused("a build needs a candidate to name".into()));
        }
        ask_db(&surql::built_against(&candidate, &part, version)).map(|_| ())
    }

    fn composable(candidates: Vec<String>) -> Result<Vec<String>, RegistryError> {
        // One part, or none, always composes with itself.
        if candidates.len() < 2 {
            return Ok(Vec::new());
        }
        let rows = ask_db(&surql::builds(&candidates))?;
        let builds: Vec<(String, String, u32)> = rows
            .iter()
            .map(|r| {
                (
                    surql::id_of(r),
                    r["part"].as_str().unwrap_or_default().to_string(),
                    r["version"].as_u64().unwrap_or(0) as u32,
                )
            })
            .collect();
        // A candidate nobody recorded is not composable either: it was built
        // against something, and not knowing what is worse than knowing it differs.
        let mut out: Vec<String> = candidates
            .iter()
            .filter(|c| !builds.iter().any(|(id, _, _)| id == *c))
            .map(|c| format!("{c} has no recorded contract version"))
            .collect();
        out.extend(surql::disagreements(&builds));
        Ok(out)
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_version_is_claimed_atomically_and_never_read_then_written() {
        // Two parts granting amendments at one boundary is the normal case, so the
        // counter increments in the database and hands back what it became.
        assert_eq!(surql::next_version(), "UPSERT contract_seq:⟨1⟩ SET n += 1 RETURN n;");
    }

    #[test]
    fn what_a_part_builds_against_is_the_latest_ratified_version() {
        let s = surql::current_contract();
        assert!(s.contains("canonical = true"), "a proposed amendment is not what siblings build on: {s}");
        assert!(s.contains("ORDER BY version DESC LIMIT 1"), "{s}");
    }

    #[test]
    fn ratifying_carries_its_ownership_guard_in_the_statement() {
        let s = surql::ratify(4, "backend");
        assert!(s.contains(r#"WHERE owner = "backend""#), "a part must not ratify another's: {s}");
    }

    #[test]
    fn answering_carries_its_once_only_guard_in_the_statement() {
        let s = surql::answer("abc", "granted", "{}", 5);
        assert!(s.contains("WHERE answered != true"), "two boundaries could resolve one request: {s}");
        assert!(s.contains("RETURN id"), "an empty result is how the caller learns it lost: {s}");
    }

    #[test]
    fn a_gate_that_did_not_pass_cannot_make_an_amendment_canonical() {
        assert!(passed(1));
        assert!(!passed(0), "no score is not a pass");
        assert!(!passed(-1));
    }

    #[test]
    fn a_retried_ask_is_one_request_and_a_new_subject_is_another() {
        let a = surql::request_id("frontend", "backend", "SearchResult needs total_pages", 3);
        let b = surql::request_id("frontend", "backend", "SearchResult needs total_pages", 3);
        assert_eq!(a, b, "asking twice in one generation is one request");
        // A different version is a different question: the interface moved under it.
        assert_ne!(a, surql::request_id("frontend", "backend", "SearchResult needs total_pages", 4));
        assert_ne!(a, surql::request_id("backend", "frontend", "SearchResult needs total_pages", 3));
    }

    #[test]
    fn a_disagreement_names_both_sides() {
        let agreed = vec![
            ("c1".to_string(), "backend".to_string(), 4u32),
            ("c2".to_string(), "frontend".to_string(), 4),
        ];
        assert!(surql::disagreements(&agreed).is_empty(), "one version composes");

        let split = vec![
            ("c1".to_string(), "backend".to_string(), 4u32),
            ("c2".to_string(), "frontend".to_string(), 3),
        ];
        let out = surql::disagreements(&split);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("frontend (c2) built against contract v3"), "{}", out[0]);
        assert!(out[0].contains("backend (c1) against v4"), "{}", out[0]);
    }

    #[test]
    fn an_unreadable_verdict_is_a_refusal_and_never_a_grant() {
        assert!(matches!(verdict_of("granted"), Verdict::Granted));
        assert!(matches!(verdict_of("counter"), Verdict::Counter));
        assert!(matches!(verdict_of(""), Verdict::Denied));
        assert!(matches!(verdict_of("GRANTED"), Verdict::Denied), "a decoding gap must not amend");
    }

    #[test]
    fn a_contract_body_cannot_carry_surrealql_syntax() {
        let s = surql::put_contract(2, r#"{"routes":["/api/x"]}; DROP TABLE contract;"#, false, "be", "r1");
        assert!(
            s.contains(r#"body = "{\"routes\":[\"/api/x\"]}; DROP TABLE contract;""#),
            "the body is a literal, not syntax: {s}"
        );
    }

    /// Captured from v3.1.3: an `ORDER BY` over a field the projection does not
    /// carry is a PARSE error, not a silently unsorted result. Both statements that
    /// sort therefore select everything they sort by, and this pins that.
    #[test]
    fn a_part_is_offered_only_its_own_undemonstrated_proposal() {
        let s = surql::proposed_for("backend");
        assert!(s.contains("canonical = false"), "a ratified version is not a proposal: {s}");
        assert!(s.contains(r#"owner = "backend""#), "and not somebody else's: {s}");
        assert!(s.contains("ORDER BY version DESC LIMIT 1"), "the latest of them: {s}");
    }

    #[test]
    fn every_statement_that_sorts_projects_what_it_sorts_by() {
        let pending = surql::pending("backend");
        assert!(pending.contains("SELECT *"), "ORDER BY at_version needs it projected: {pending}");
        assert!(pending.contains("ORDER BY at_version"), "{pending}");
        // Unanswered is `!= true`, not `= false`: a fresh request has no such
        // field at all, because writing one on every ask would un-answer a verdict
        // the part has already been given.
        assert!(pending.contains("answered != true"), "{pending}");
        assert!(
            !surql::put_request("i", "a", "b", "s", "x", 1).contains("answered ="),
            "asking again must not reset an answer"
        );
        let current = surql::current_contract();
        assert!(current.contains("SELECT *"), "{current}");
        assert!(current.contains("ORDER BY version DESC"), "{current}");
    }

    #[test]
    fn an_empty_registry_reads_as_empty_rather_than_broken() {
        // The first `current()` of a run always precedes the first `publish`.
        let absent = r#"[{"status":"ERR","result":"The table 'contract' does not exist"}]"#;
        assert!(surql::rows(absent).unwrap().is_empty());
        assert!(surql::rows(r#"[{"status":"ERR","result":"permission denied"}]"#).is_err());
    }

    #[test]
    fn a_request_reads_back_with_the_version_it_was_asked_at() {
        let row = json!({
            "id": "request:`9f3a`", "from_part": "frontend", "to_part": "backend",
            "subject": "SearchResult needs total_pages",
            "body": "I cannot paginate from next_cursor alone",
            "at_version": 3, "answered": true, "verdict": "counter",
            "answer": "use has_more; total pages costs a COUNT on every query"
        });
        let r = request_of(&row);
        assert_eq!(r.id, "9f3a", "the id round-trips whatever quoting the server chose");
        assert_eq!(r.at_version, 3);
        assert!(matches!(r.verdict, Verdict::Counter));
        assert!(r.answer.contains("has_more"));
    }

    #[test]
    fn a_candidate_with_no_recorded_version_is_not_composable() {
        // Not knowing what something was built against is worse than knowing it
        // differs, so it is reported rather than assumed to agree.
        let rows = vec![("c1".to_string(), "backend".to_string(), 4u32)];
        assert!(surql::disagreements(&rows).is_empty(), "one build cannot disagree with itself");
    }
}
