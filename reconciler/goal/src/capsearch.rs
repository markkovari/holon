//! "Do we already have something for this?" — asked of the catalogue, answered
//! without a model.
//!
//! ADR-0089's first gap: reuse is ENFORCED (a gate fails a part that reimplements `auth-guard`) but
//! never DISCOVERED; every new goal needs somebody who already knows what the pool contains (150
//! components, 93 exported interfaces).
//!
//! Deliberately not embeddings: a capability is an identifier plus one sentence, term overlap suits
//! that shape, costs nothing, runs in a millisecond, and tests by asserting "hash a password" returns
//! `auth-guard`. The embedding path in `knowledge-memory` would make this its first caller.
//!
//! What makes it more than grep is the GRAPH: two components can match equally while one ships in
//! twenty apps and the other in none, and only the graph knows.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use comp_fleet::plug::Catalog;

/// A capability, as a searcher needs to see it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capability {
    pub name: String,
    /// One sentence, from the component's own `//!` line.
    ///
    /// Weaker than it sounds: 57 of the 109 entries say only "`x` — reference implementation of `x:y`",
    /// which matches nothing a caller would type. That is why `doc` exists.
    pub description: String,
    /// The component's own WIT, as prose: written next to the contract, it says what the thing DOES
    /// (`rate-limiter`'s catalogue line is a tautology, its WIT opens "a small, generic rate-limit /
    /// lockout capability …"). Derived rather than authored, so it cannot drift from the contract.
    pub doc: String,
    pub exports: Vec<String>,
    /// An application, not a capability.
    ///
    /// Nobody reuses `jobs-domain`; they reuse `lock-mutex`, yet an app's name matches as well as anything.
    /// So apps rank BELOW every capability that matched rather than being deleted: an app is still
    /// evidence that a composition exists (ADR-0089).
    pub is_app: bool,
    /// How many applications carry it today: the tie-breaker that matters, since a capability twenty apps
    /// compose is a safer answer than one nothing uses.
    pub apps: usize,
}

/// Everything the components and the graph know, joined.
///
/// The COMPONENTS supply the prose, the artifacts the exports and blast radius. Neither alone is
/// enough: a source has a description but not what it resolves to; an artifact has the interfaces but
/// not what anything is FOR.
///
/// Both rules (description, whether it is an application) live in `crate::catalogue` and are applied
/// here directly instead of reading the 500 KB generated `components/catalog.json`, so they cannot
/// go stale.
pub fn capabilities(
    repo_root: &Path,
    catalog: &Catalog,
    apps_of: &BTreeMap<String, usize>,
) -> Vec<Capability> {
    // Not a name list: a component that exports nothing outside `wasi:` offers no contract, so nothing
    // can plug it.
    let app_only: BTreeSet<String> = catalog
        .names()
        .map(String::from)
        .collect::<Vec<_>>()
        .into_iter()
        .filter(|n| {
            catalog
                .surface(n)
                .map(|s| !crate::catalogue::offers_a_contract(&s.exports))
                .unwrap_or(false)
        })
        .collect();

    let described: BTreeMap<String, String> = catalog
        .names()
        .map(String::from)
        .collect::<Vec<_>>()
        .into_iter()
        .map(|name| {
            let doc = crate::catalogue::first_doc_line(
                &repo_root.join("components").join(&name).join("src/lib.rs"),
            );
            (name, doc)
        })
        .collect();

    let wit = wit_prose(repo_root);
    let mut out: Vec<Capability> = Vec::new();
    for name in catalog.names().map(String::from).collect::<Vec<_>>() {
        let Some(surface) = catalog.surface(&name) else { continue };
        // Something that exports nothing cannot be reached for. An application is not a capability.
        if surface.exports.is_empty() {
            continue;
        }
        // The prose belongs to the PACKAGE, not the crate: `auth-guard` has no wit directory of its own, so
        // the text is found by what the component exports.
        let doc = surface
            .exports
            .iter()
            .filter_map(|e| e.split('/').next())
            .find_map(|pkg| wit.get(pkg).cloned())
            .unwrap_or_default();
        out.push(Capability {
            description: described.get(&name).cloned().unwrap_or_default(),
            doc,
            exports: surface.exports.iter().cloned().collect(),
            apps: apps_of.get(&name).copied().unwrap_or(0),
            is_app: app_only.contains(&name),
            name,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Every WIT package in the tree, and the text that describes it.
///
/// The HEADER comment only (the block above `package`), not the whole file: indexing whole files made
/// long WITs win on common words ("stop a caller making too many requests" returned
/// `anthropic-provider` first). The header says what the thing is FOR and is about the same length
/// for every package, so a bag of words over it is a fair comparison.
fn wit_prose(repo_root: &Path) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut dirs = vec![repo_root.join("wit")];
    if let Ok(entries) = std::fs::read_dir(repo_root.join("components")) {
        dirs.extend(entries.flatten().map(|e| e.path().join("wit")));
    }
    for dir in dirs {
        let Ok(files) = std::fs::read_dir(&dir) else { continue };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().is_none_or(|e| e != "wit") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Some(pkg) = text
                .lines()
                .find_map(|l| l.trim().strip_prefix("package ").and_then(|r| r.split('@').next()))
            else {
                continue;
            };
            let header: String = text
                .lines()
                .take_while(|l| !l.trim_start().starts_with("package "))
                .filter_map(|l| l.trim_start().strip_prefix("//"))
                .collect::<Vec<_>>()
                .join(" ");
            out.entry(pkg.trim().trim_end_matches(';').to_string()).or_default().push_str(&header);
        }
    }
    out
}

/// Words worth matching on. Everything else is noise in a one-line description.
///
/// The list grew after "two workers must not do the same job" matched 57 of 152 capabilities and
/// ranked `outbox` first on "two", "not" and "same". Terms shorter than three characters are dropped
/// separately, which is why "do", "be" and "no" are not here.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "as", "at", "by", "for", "from", "in", "into", "is", "it", "of", "on", "or",
    "that", "the", "to", "with", "need", "needs", "want", "i", "we", "my", "this", "not", "two",
    "same", "must", "does", "doing", "when", "what", "which", "than", "then", "them", "they",
    "you", "your", "its", "has", "have", "had", "will", "would", "could", "should", "are", "was",
    "were", "been", "but", "also", "just", "only", "very", "much", "more", "most", "such", "each",
    "both", "either", "neither",
];

fn terms(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2 && !STOPWORDS.contains(t))
        .map(str::to_string)
        .collect()
}

/// How well a capability answers a query, and why.
#[derive(Debug, Clone)]
pub struct Match<'a> {
    pub capability: &'a Capability,
    pub score: f64,
    /// The terms that matched, so a caller can say WHY: a ranking nobody can check is a ranking nobody
    /// should trust.
    pub because: Vec<String>,
}

/// Search the catalogue.
///
/// Scoring, in order of weight and stated plainly because a hidden ranking is worse than none:
///
/// * a term in the component's NAME or an exported interface name is worth most;
/// * a term in the description is worth less, since a sentence mentions things it merely relates to;
/// * ties break towards what applications already carry.
pub fn find<'a>(query: &str, pool: &'a [Capability]) -> Vec<Match<'a>> {
    let wanted = terms(query);
    if wanted.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<Match<'a>> = Vec::new();
    for cap in pool {
        let name_terms = terms(&cap.name);
        let iface_terms: Vec<String> = cap.exports.iter().flat_map(|e| terms(e)).collect();
        let desc_terms: Vec<String> =
            terms(&cap.description).into_iter().chain(terms(&cap.doc)).collect();

        let mut score = 0.0;
        let mut because = Vec::new();
        for term in &wanted {
            // Prefix rather than equality: "hashing" should find "hash", and a description writer did not know
            // which form a caller would type.
            let hit_name = name_terms.iter().any(|t| t.starts_with(term) || term.starts_with(t));
            let hit_iface = iface_terms.iter().any(|t| t.starts_with(term) || term.starts_with(t));
            let hit_desc = desc_terms.iter().any(|t| t.starts_with(term) || term.starts_with(t));
            if hit_name || hit_iface {
                score += 3.0;
                if !because.contains(term) {
                    because.push(term.clone());
                }
            } else if hit_desc {
                score += 1.0;
                if !because.contains(term) {
                    because.push(term.clone());
                }
            }
        }
        // ONE shared word is a coincidence, not an answer: "diagnose a patient from radiology imagery"
        // reached `image-optimizer` because "imagery" prefix-matches "image" in its name. Two distinct terms
        // is the bar; a single-term query is exempt ("slugify" is a whole question).
        if score == 0.0 || (wanted.len() > 1 && because.len() < 2) {
            continue;
        }
        // Graph centrality multiplier: log-ish adoption score so that widely composed capabilities break
        // ties towards vetted infrastructure.
        score += (cap.apps as f64 + 1.0).ln() * 0.5;
        out.push(Match { capability: cap, score, because });
    }
    // Capabilities first, applications after: a showcase never outranks the thing it is built from.
    out.sort_by(|a, b| {
        a.capability
            .is_app
            .cmp(&b.capability.is_app)
            .then(b.score.total_cmp(&a.score))
            .then(a.capability.name.cmp(&b.capability.name))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> Vec<Capability> {
        vec![
            Capability {
                name: "csv".into(),
                description: "Parse and format delimited (CSV/TSV) text".into(),
                doc: String::new(),
                exports: vec!["csv:codec/codec@0.1.0".into()],
                apps: 3,
                is_app: false,
            },
            Capability {
                name: "auth-guard".into(),
                description: "Reference implementation of the auth:identity contract".into(),
                doc: "issue a session, verify a password".into(),
                exports: vec!["auth:identity/accounts@0.1.0".into()],
                apps: 18,
                is_app: false,
            },
            Capability {
                name: "search-index".into(),
                description: "Generic full-text inverted-index capability with ranked results"
                    .into(),
                doc: String::new(),
                exports: vec!["search:index/index@0.1.0".into()],
                apps: 4,
                is_app: false,
            },
            Capability {
                name: "lonely".into(),
                description: "Nothing to do with any of it".into(),
                doc: String::new(),
                exports: vec!["lonely:thing/x@0.1.0".into()],
                apps: 0,
                is_app: false,
            },
        ]
    }

    #[test]
    fn a_query_finds_the_capability_that_answers_it() {
        let p = pool();
        for (query, want) in [
            ("I need to format rows as CSV", "csv"),
            ("full-text search with ranking", "search-index"),
            ("accounts and identity", "auth-guard"),
        ] {
            let hits = find(query, &p);
            assert_eq!(
                hits.first().map(|m| m.capability.name.as_str()),
                Some(want),
                "{query:?} should find {want}, got {:?}",
                hits.iter().map(|m| &m.capability.name).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn a_query_about_nothing_we_have_returns_nothing() {
        // The answer that matters most, because it says "build it": a searcher that always returns its
        // closest row would send every goal to reuse something unrelated.
        assert!(find("orbital mechanics for a satellite", &pool()).is_empty());
    }

    #[test]
    fn the_reason_is_reported_with_the_score() {
        let p = pool();
        let hits = find("csv", &p);
        assert_eq!(hits[0].because, vec!["csv".to_string()], "a hit says which term matched");
    }

    #[test]
    fn use_breaks_a_tie_but_does_not_decide_the_answer() {
        let mut p = pool();
        // A far better textual match with no users must still win over a weak match twenty apps carry.
        p.push(Capability {
            name: "csv-pretty".into(),
            description: "Format CSV tables for humans, with aligned columns".into(),
            doc: String::new(),
            exports: vec!["csv:pretty/render@0.1.0".into()],
            apps: 0,
            is_app: false,
        });
        let hits = find("csv", &p);
        assert!(
            hits.iter().take(2).any(|m| m.capability.name == "csv-pretty"),
            "an unused but well-matching capability must still surface: {:?}",
            hits.iter().map(|m| (&m.capability.name, m.score)).collect::<Vec<_>>()
        );
    }
}

/// Two components exporting the same interface.
///
/// ADR-0089's last unbuilt gap. Prevention (consulting `capsearch` before writing) has no feedback
/// loop; this counts what got through.
///
/// Structural, not semantic: "exports the same interface" is a fact the catalogue holds, needing no
/// model or descriptions. It is also what `comp-plug` decides on silently (`exporters` uses
/// `or_insert_with`, so the first-sorting one wins); this makes that visible.
///
/// A pair is NOT necessarily a mistake (`record-store` and a mock both export `records:store/store`
/// on purpose): this reports pairs and how alike they are; deciding is a person's job.
#[derive(Debug, Clone, PartialEq)]
pub struct Twins {
    pub interface: String,
    pub components: Vec<String>,
    /// Interfaces exported by all of them, the overlap. It separates a duplicate from an alternative: two
    /// components agreeing on ONE interface may be a reference and its mock, on their whole surface the
    /// same component written twice.
    pub shared: Vec<String>,
    pub overlap: f64,
}

/// Every interface exported by more than one component in the catalogue, sorted by overlap.
pub fn twins(catalog: &Catalog) -> Vec<Twins> {
    let mut by_interface: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in catalog.names() {
        let Some(surface) = catalog.surface(name) else { continue };
        for export in &surface.exports {
            by_interface.entry(export.clone()).or_default().push(name.to_string());
        }
    }

    let mut out: Vec<Twins> = Vec::new();
    for (interface, components) in by_interface {
        if components.len() < 2 {
            continue;
        }
        let surfaces: Vec<&BTreeSet<String>> =
            components.iter().filter_map(|c| catalog.surface(c)).map(|s| &s.exports).collect();
        let Some(first) = surfaces.first() else { continue };
        let shared: BTreeSet<String> =
            first.iter().filter(|e| surfaces.iter().all(|s| s.contains(*e))).cloned().collect();
        // Against the SMALLEST surface: a tiny component fully contained in a large one is entirely
        // duplicated, which dividing by the larger would hide.
        let smallest = surfaces.iter().map(|s| s.len()).min().unwrap_or(1).max(1);
        out.push(Twins {
            interface,
            components,
            shared: shared.iter().cloned().collect(),
            overlap: shared.len() as f64 / smallest as f64,
        });
    }
    out.sort_by(|a, b| b.overlap.total_cmp(&a.overlap).then(a.interface.cmp(&b.interface)));
    out
}
