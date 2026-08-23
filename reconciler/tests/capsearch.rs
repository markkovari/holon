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
    // Added when the 57 tautological descriptions were replaced with sentences a
    // CALLER would write. Every one of these failed beforehand, and none of them
    // needed a change to the searcher — only to what it was searching.
    ("stop a caller making too many requests", "rate-limiter"),
    ("record who did what and when", "audit-log"),
    ("make a mutating request safe to retry", "idempotency-guard"),
    ("mask personal data before it reaches a log", "pii-redact"),
    ("turn a title into a url", "slug"),
    ("do this later, on a timer", "scheduler-timer"),
    ("the books have to balance", "ledger"),
    ("render an email from a template", "email-render"),
    // In the top 3 rather than first: `event-bus` still wins on "event" matching
    // its name. Kept as a positive because "reachable" is the thing a branch
    // needs, and the ranking flaw is recorded in KNOWN_MISSES below.
    ("expand a repeating event", "rrule"),
];

/// Questions this searcher gets WRONG, kept here on purpose.
///
/// The original entry — "stop a caller making too many requests" — is gone,
/// because it now passes. Nothing about the searcher changed; `rate-limiter`'s
/// description stopped being "reference implementation of `ratelimit:guard`" and
/// started being a sentence about what a caller wants. 57 descriptions were like
/// that, and this list is what survived fixing them.
///
/// Both survivors have the SAME cause, and it is not vocabulary: a match on a
/// component's NAME or interface scores 3, a match on its description scores 1 —
/// so any query using a word that happens to appear in some component's name
/// outranks a component that actually does the thing.
///
/// It bites hardest because the pool contains 33 DOMAIN APPS. Nobody reuses
/// `jobs-domain`; they reuse `lock-mutex`. An application is not a capability, and
/// having them in the same pool is what lets "job" reach a showcase before it
/// reaches the mutex. The fix is the app-local tier — apps are not in the
/// catalogue at all — not a synonym list and not a weight somebody tuned until
/// these two passed.
const KNOWN_MISSES: &[(&str, &str)] = &[
    // `jobs-domain` wins on "job" matching its name.
    ("two workers must not do the same job", "lock-mutex"),
];

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

/// Interfaces two components are SUPPOSED to export, and why.
///
/// Every one of these is a substitution point — the reason a WIT interface exists
/// at all. What the audit is looking for is the entry nobody put here.
const DELIBERATE: &[(&str, &str)] = &[
    (
        "durable:workflow/orchestrator",
        "two backends for the same contract: `golem-bridge` runs it on a real Golem \
         worker, `inproc-workflow` in-process, and the app cannot tell",
    ),
    (
        "graph:fitness/evaluator",
        "`checks-runner` shells out to the project's own commands; `mock-fitness` \
         returns a scripted verdict so the loop can be tested without a gate",
    ),
    (
        "llm:inference/inference",
        "the provider boundary: anthropic, openai, a local `llm-inference` and a \
         mock. Swapping the model is a composition, which is the whole point",
    ),
    (
        "ui:assets/files",
        "one bundle per SPA — `console-assets`, `track-assets` — plus the generic \
         `static-assets`. These are app-local by nature (ADR-0089): a bundle is \
         wanted by exactly the app it was built for",
    ),
];

/// Nobody built the same thing twice.
///
/// ADR-0089's last unbuilt gap, in the cheapest form that works. Prevention is the
/// real mechanism — `capsearch` consulted before writing — but a working search and
/// a broken one look identical from outside, so this counts what got through.
///
/// Structural on purpose. "Exports the same interface" is a fact the catalogue
/// already holds; it needs no model and no descriptions, which matters because 57
/// of the 109 descriptions are tautologies.
#[test]
fn the_same_capability_was_not_built_twice() {
    let root = repo_root();
    let catalog = Catalog::scan(&default_dirs(&root));
    if catalog.is_empty() {
        eprintln!("SKIPPED: nothing is built, so no duplicate could be seen. `just build` first.");
        return;
    }

    let mut undeclared = Vec::new();
    for t in comp_reconciler::capsearch::twins(&catalog) {
        // A standard interface exported by everything is the shape of the system,
        // not a duplicate — and `wasi:http/incoming-handler`'s 65 exporters are
        // the population that would stop owning their own entry point, which is a
        // design question rather than a mistake.
        if t.interface.starts_with("wasi:") {
            continue;
        }
        let bare = t.interface.split('@').next().unwrap_or(&t.interface);
        if DELIBERATE.iter().any(|(iface, _)| *iface == bare) {
            continue;
        }
        undeclared.push(format!("  {} exported by {}", t.interface, t.components.join(", ")));
    }

    assert!(
        undeclared.is_empty(),
        "these interfaces have more than one exporter and nothing says why:\n{}\n\nEither the \
         capability was built twice — in which case `comp-capgraph --find` should have found \
         the first one, and why it did not is the interesting question — or it is a deliberate \
         substitution point, in which case add it to DELIBERATE with the reason. Note that \
         `comp-plug` resolves an interface to ONE exporter by scan order, so today one of \
         them silently wins every composition.",
        undeclared.join("\n")
    );
}

/// A capability describes itself in a caller's words, not its own.
///
/// 57 of 109 catalogue entries once read "`x` — reference implementation of
/// `x:y`", which is true, tautological, and matches nothing anybody would type.
/// `rate-limiter` shipped in 22 applications and could not be found by "stop a
/// caller making too many requests", because the words it used were "counts
/// failures against an opaque key".
///
/// The description is generated from the FIRST `//!` line of `src/lib.rs`
/// (`tools/gen-catalog.py`), so that line is what this checks — the place a person
/// writes, rather than the generated file they would have to remember to rebuild.
///
/// Deliberately blunt. It does not judge whether a description is *good*; it
/// refuses the one form that is known to be useless. A rule that tried to score
/// prose would be argued with, and this one cannot be: restating your own name is
/// not a description.
#[test]
fn a_capability_does_not_describe_itself_as_its_own_reference_implementation() {
    let root = repo_root();
    let Ok(dirs) = std::fs::read_dir(root.join("components")) else {
        panic!("no components/ directory");
    };
    let mut tautological = Vec::new();
    let mut checked = 0usize;

    for entry in dirs.flatten() {
        let lib = entry.path().join("src/lib.rs");
        let Ok(text) = std::fs::read_to_string(&lib) else { continue };
        let Some(first) = text.lines().find(|l| l.starts_with("//!")) else { continue };
        checked += 1;
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = first.to_lowercase();
        if lower.contains("reference implementation") {
            tautological.push(format!("  {name}: {}", first.trim()));
        }
    }

    assert!(checked > 100, "only read {checked} descriptions — the walk is wrong");
    assert!(
        tautological.is_empty(),
        "{} component(s) describe themselves as a reference implementation of their own \
         interface, which is a sentence nobody searching would type:\n{}\n\nWrite what a \
         CALLER wants — \"stop a caller making too many requests\", not \"reference \
         implementation of `ratelimit:guard`\". The WIT header below it can stay technical; \
         this one line is the searchable one.",
        tautological.len(),
        tautological.join("\n")
    );
    println!("  {checked} component descriptions, none tautological");
}

/// A component with NO description is invisible to a caller's words.
///
/// ADR-0094 made the first `//!` line the searchable one and lints the one form
/// known to be useless — describing yourself as a reference implementation of
/// your own interface. It could not catch the other useless form, because the
/// walk `continue`s on a crate with no `//!` line at all: a component that says
/// nothing was skipped rather than failed.
///
/// 31 of them had accumulated that way. `capsearch` still had them in the pool
/// via artifact reflection, so the loop knew they EXISTED and had no prose to
/// match a goal against — which is the same as not having them, for the one
/// question the pool is asked.
///
/// Blunt in the same way as its sibling, and for the same reason: this does not
/// judge whether a description is good. "You wrote none" is not a rule anybody
/// can argue with.
#[test]
fn every_component_has_a_description() {
    let root = repo_root();
    let Ok(dirs) = std::fs::read_dir(root.join("components")) else {
        panic!("no components/ directory");
    };
    let mut silent = Vec::new();
    let mut checked = 0usize;

    for entry in dirs.flatten() {
        let manifest = entry.path().join("Cargo.toml");
        let Ok(toml) = std::fs::read_to_string(&manifest) else { continue };
        // Only components. A plain library crate — `semver-range`, `capman` —
        // is not in the pool and has nothing to be findable by.
        if !toml.contains("[package.metadata.component]") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path().join("src/lib.rs")) else { continue };
        checked += 1;
        let has_prose = text
            .lines()
            .find(|l| l.starts_with("//!"))
            .map(|l| !l[3..].trim().is_empty())
            .unwrap_or(false);
        if !has_prose {
            silent.push(format!("  {}", entry.file_name().to_string_lossy()));
        }
    }

    assert!(checked > 100, "only read {checked} components — the walk is wrong");
    assert!(
        silent.is_empty(),
        "{} component(s) have no `//!` description, so nothing a caller types can \
         reach them:\n{}\n\nWrite one line saying what a CALLER wants from it. \
         `tools/gen-catalog.py` lifts that line into `catalog.json`, and \
         `capsearch` matches a goal against it.",
        silent.len(),
        silent.join("\n")
    );
    println!("  {checked} components, all described");
}

