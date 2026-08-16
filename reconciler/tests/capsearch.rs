//! "Do we already have something for this?", asked of the real catalogue.
//!
//! The unit tests in `capsearch.rs` pin the scoring against a fixture. This asks
//! the question a goal would actually ask, of the 150 components that are really
//! here, and it exists as much to record where the answer is WRONG as where it is
//! right.
//!
//! No model, and deliberately so (ADR-0089): term overlap over each package's WIT
//! header, with the capability graph breaking ties towards what applications
//! already carry. It costs nothing and runs in a millisecond, which is what makes
//! it something a planner can call on every goal.

use std::collections::BTreeMap;

use comp_reconciler::capsearch::{capabilities, find, Capability};
use comp_reconciler::fleet::repo_root;
use comp_reconciler::plug::{default_dirs, Catalog};

/// Questions somebody would really type, and the component that answers them.
///
/// Phrased as a person describes a need, not as the component describes itself —
/// otherwise this tests that a name matches itself.
const SHOULD_FIND: &[(&str, &str)] = &[
    ("hash a password and issue a session token", "auth-guard"),
    ("rank documents by relevance", "search-index"),
    ("store records and find them by a field", "record-store"),
    ("format some rows as CSV", "csv"),
    ("generate a unique identifier", "id-generate"),
];

/// Questions this searcher gets WRONG, kept here on purpose.
///
/// Each is a vocabulary mismatch: the asker's words and the contract's words mean
/// the same thing and share no characters, which is the one thing term overlap
/// cannot do. `rate-limiter` ships in 22 applications and its WIT opens "a small,
/// generic rate-limit / lockout capability … counts failures against an opaque key"
/// — nothing in there is "too many requests".
///
/// This is the trigger for the embedding path, not a bug to paper over with
/// synonyms: a hand-written thesaurus is a second corpus to maintain and it would
/// be wrong in a different way. `knowledge-memory` already embeds and does KNN in
/// SurrealDB, so the fix is to make this its first caller (ADR-0089 slice 1).
///
/// If one of these starts passing, delete it from here — the searcher got better
/// and the list should say so.
const KNOWN_MISSES: &[(&str, &str)] = &[("stop a caller making too many requests", "rate-limiter")];

fn pool() -> Option<Vec<Capability>> {
    let root = repo_root();
    let catalog = Catalog::scan(&default_dirs(&root));
    if catalog.is_empty() {
        eprintln!("SKIPPED: nothing is built, so no capability was searched. `just build` first.");
        return None;
    }
    // App counts are the tie-breaker; an empty map only removes the nudge.
    let mut apps_of: BTreeMap<String, usize> = BTreeMap::new();
    for name in catalog.names().map(String::from).collect::<Vec<_>>() {
        for part in catalog.closure(&name) {
            *apps_of.entry(part).or_default() += 1;
        }
    }
    Some(capabilities(&root, &catalog, &apps_of))
}

#[test]
fn a_plain_question_finds_the_capability_that_answers_it() {
    let Some(pool) = pool() else { return };
    assert!(pool.len() > 50, "only {} capabilities — the catalogue did not load", pool.len());

    let mut wrong = Vec::new();
    for (query, want) in SHOULD_FIND {
        let hits = find(query, &pool);
        let top: Vec<&str> = hits.iter().take(3).map(|m| m.capability.name.as_str()).collect();
        if !top.contains(want) {
            wrong.push(format!("  {query:?}\n      wanted {want} in the top 3, got {top:?}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "the catalogue could not answer questions it has answers for:\n{}",
        wrong.join("\n")
    );
    println!("  {} capabilities searchable; {} questions answered", pool.len(), SHOULD_FIND.len());
}

#[test]
fn the_known_misses_are_still_missing() {
    let Some(pool) = pool() else { return };
    let mut fixed = Vec::new();
    for (query, want) in KNOWN_MISSES {
        let hits = find(query, &pool);
        if hits.iter().take(3).any(|m| m.capability.name == *want) {
            fixed.push(format!("  {query:?} now finds {want}"));
        }
    }
    assert!(
        fixed.is_empty(),
        "these are listed as known misses and no longer miss — the searcher improved, \
         so remove them from KNOWN_MISSES rather than leaving a test that lies:\n{}",
        fixed.join("\n")
    );
}

/// The most valuable answer a searcher gives is "nothing".
///
/// A search that always returns its closest row sends every goal off to reuse
/// something unrelated, which is worse than not searching: the whole point of
/// asking is to decide between REUSE and BUILD (ADR-0089), and a confident wrong
/// answer makes that decision badly.
#[test]
fn a_question_the_catalogue_cannot_answer_returns_nothing() {
    let Some(pool) = pool() else { return };
    for query in [
        "orbital mechanics for a satellite constellation",
        "diagnose a patient from radiology imagery",
    ] {
        let hits = find(query, &pool);
        assert!(
            hits.is_empty(),
            "{query:?} matched {:?} — a searcher that answers everything answers nothing",
            hits.iter().take(3).map(|m| &m.capability.name).collect::<Vec<_>>()
        );
    }
}
