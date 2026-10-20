//! The capsearch questions a goal would ask, against the real catalogue, misses included.

use std::collections::BTreeMap;

use comp_fleet::fleet::repo_root;
use comp_fleet::plug::{default_dirs, Catalog};
use comp_goal::capsearch::{capabilities, find, Capability};

/// Phrased as a person describes a need, or this tests that a name matches itself.
const SHOULD_FIND: &[(&str, &str)] = &[
    ("hash a password and issue a session token", "auth-guard"),
    ("rank documents by relevance", "search-index"),
    ("store records and find them by a field", "record-store"),
    ("format some rows as CSV", "csv"),
    ("generate a unique identifier", "id-generate"),
    ("stop a caller making too many requests", "rate-limiter"),
    ("record who did what and when", "audit-log"),
    ("make a mutating request safe to retry", "idempotency-guard"),
    ("mask personal data before it reaches a log", "pii-redact"),
    ("turn a title into a url", "slug"),
    ("do this later, on a timer", "scheduler-timer"),
    ("the books have to balance", "ledger"),
    ("render an email from a template", "email-render"),
    // Top 3, not first: `event-bus` wins on "event" in its name.
    ("expand a repeating event", "rrule"),
];

/// Kept on purpose: a name match (3) outranks a description match (1); needs an app-local tier.
const KNOWN_MISSES: &[(&str, &str)] = &[
    // `jobs-domain` wins on "job" matching its name.
    ("two workers must not do the same job", "lock-mutex"),
];

fn pool() -> Option<Vec<Capability>> {
    let root = repo_root();
    let catalog = Catalog::scan(&default_dirs(&root));
    if catalog.is_empty() {
        eprintln!("SKIPPED: nothing is built, so no capability was searched. `cargo xtask build --force` first.");
        return None;
    }
    // An empty map only removes the tie-break nudge.
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

/// "Nothing" is the most valuable answer: the closest row would spoil reuse vs build (ADR-0089).
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

/// Each is a substitution point; the audit looks for the entry nobody put here.
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
        "jev:decision/decision",
        "the same provider boundary for discrete decisions: `typesafe-provider` is \
         the real Jev API, `jev-decision` a trivial reference impl (mirrors \
         `llm-inference`'s own built-in mock), `mock-jev-provider` a scripted one \
         for deterministic tests. Swapping the provider is a composition",
    ),
    (
        "ui:assets/files",
        "one bundle per SPA — `console-assets`, `track-assets` — plus the generic \
         `static-assets`. These are app-local by nature (ADR-0089): a bundle is \
         wanted by exactly the app it was built for",
    ),
    (
        "actor:entity/handler",
        "actor instances implement this handler interface to process messages and \
         lifecycle events. Each actor type (like ticket-actor) has its own implementation.",
    ),
];

/// A working search and a broken one look identical from outside, so count what got through.
#[test]
fn the_same_capability_was_not_built_twice() {
    let root = repo_root();
    let catalog = Catalog::scan(&default_dirs(&root));
    if catalog.is_empty() {
        eprintln!("SKIPPED: nothing is built, so no duplicate could be seen. `cargo xtask build --force` first.");
        return;
    }

    let mut undeclared = Vec::new();
    for t in comp_goal::capsearch::twins(&catalog) {
        // `wasi:` is the system's shape, not a duplicate (65 exporters of incoming-handler).
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

/// Refuses the one useless form (restating your own name), checked on the first `//!` line.
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

/// Without a `//!` line a component exists for capsearch but has no prose to match.
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
        // A plain library crate (`semver-range`, `capman`) is not in the pool.
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
         `comp-catalog` reads that line straight off the component, and \
         `capsearch` matches a goal against it.",
        silent.len(),
        silent.join("\n")
    );
    println!("  {checked} components, all described");
}

/// `comp-catalog` reads sources and `capsearch` artifacts; both must agree on every component.
#[test]
fn the_catalogue_and_the_artifacts_agree_on_what_is_reusable() {
    let root = comp_fleet::fleet::repo_root();
    let catalog = Catalog::scan(&default_dirs(&root));
    if catalog.is_empty() {
        eprintln!("SKIPPED: nothing is built — run `cargo xtask build --force`");
        return;
    }
    // The program, not a committed snapshot.
    let Ok(out) = std::process::Command::new(env!("CARGO_BIN_EXE_comp-catalog"))
        .arg("--json")
        .current_dir(&root)
        .output()
    else {
        eprintln!("SKIPPED: comp-catalog did not run");
        return;
    };
    let entries: Vec<serde_json::Value> =
        serde_json::from_slice(&out.stdout).expect("comp-catalog --json");

    let mut wrong = Vec::new();
    let mut checked = 0usize;
    for e in &entries {
        let Some(name) = e["name"].as_str() else { continue };
        let Some(surface) = catalog.surface(name) else { continue };
        checked += 1;
        let from_artifact = comp_goal::catalogue::offers_a_contract(&surface.exports);
        let from_source = e["reusable_as_is"].as_bool().unwrap_or(false);
        if from_artifact != from_source {
            wrong.push(format!(
                "  {name}: catalogue says {from_source}, the artifact exports {:?}",
                surface.exports
            ));
        }
    }
    assert!(checked > 100, "only {checked} components checked — the join is broken");
    assert!(
        wrong.is_empty(),
        "the catalogue disagrees with the built artifacts about what can be \
         plugged:\n{}",
        wrong.join("\n")
    );
}
