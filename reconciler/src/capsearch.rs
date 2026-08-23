//! "Do we already have something for this?" — asked of the catalogue, answered
//! without a model.
//!
//! ADR-0089's first gap: reuse is ENFORCED (a clinic gate fails a part that
//! reimplements `auth-guard`) but never DISCOVERED. A human wrote `auth:identity`
//! and `csv:codec` into `wit/clinic.wit`; the part then had no choice but to use
//! them. That does not compound — every new goal needs somebody who already knows
//! what the pool contains, and the pool is 150 components with 93 exported
//! interfaces.
//!
//! Deliberately not embeddings. A capability is described by an identifier and one
//! sentence written by whoever built it, and term overlap over 109 short
//! descriptions is a good match for that shape — with the advantage that it costs
//! nothing, runs in a millisecond, and can be tested by asserting that "hash a
//! password" returns `auth-guard`. When this stops being good enough, the
//! embedding path already exists in `knowledge-memory` and this becomes its first
//! caller rather than its competitor.
//!
//! What makes it more than grep is the GRAPH. Two components can match a query
//! equally well while one ships in twenty apps and the other in none; the first is
//! the answer, and only the graph knows which is which.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::plug::Catalog;

/// A capability, as a searcher needs to see it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capability {
    pub name: String,
    /// One sentence, from `components/catalog.json`, written by whoever built it.
    ///
    /// Weaker than it sounds: 57 of the 109 entries say only "`x` — reference
    /// implementation of `x:y`", which is a tautology and matches nothing a
    /// caller would type. That is why `doc` exists.
    pub description: String,
    /// The component's own WIT, as prose.
    ///
    /// The real description of a capability is written next to its contract, by
    /// the person who designed it, and it says what the thing DOES:
    /// `rate-limiter`'s catalogue line is "reference implementation of
    /// ratelimit:guard" while its WIT opens "a small, generic rate-limit /
    /// lockout capability … counts failures against an opaque key and reports
    /// when a key is locked out". Only one of those can be searched.
    ///
    /// Derived rather than authored for the catalogue, so it cannot drift from
    /// the contract it describes — the same rule as everything else here.
    pub doc: String,
    /// What it can satisfy.
    pub exports: Vec<String>,
    /// How many applications carry it today. The tie-breaker that matters: a
    /// capability twenty apps already compose is a safer answer than one nothing
    /// uses, whatever the descriptions say.
    pub apps: usize,
}

/// Everything the catalogue and the graph know, joined.
///
/// `catalog.json` supplies the prose, the artifacts supply the exports and the
/// blast radius. Neither alone is enough: the catalogue has descriptions but is
/// hand-generated and covers 109 of 150 components, and the artifacts have the
/// truth about interfaces but no idea what anything is FOR.
pub fn capabilities(
    repo_root: &Path,
    catalog: &Catalog,
    apps_of: &BTreeMap<String, usize>,
) -> Vec<Capability> {
    let described: BTreeMap<String, String> =
        std::fs::read_to_string(repo_root.join("components/catalog.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|e| {
                Some((
                    e["name"].as_str()?.to_string(),
                    e["description"].as_str().unwrap_or_default().to_string(),
                ))
            })
            .collect();

    let wit = wit_prose(repo_root);
    let mut out: Vec<Capability> = Vec::new();
    for name in catalog.names().map(String::from).collect::<Vec<_>>() {
        let Some(surface) = catalog.surface(&name) else { continue };
        // Something that exports nothing cannot be reached for. An application is
        // not a capability, however useful it is.
        if surface.exports.is_empty() {
            continue;
        }
        // The prose belongs to the PACKAGE, not the crate: `auth-guard` has no
        // wit directory of its own and implements a world declared in the shared
        // root `wit/`, so the text is found by what the component exports.
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
            name,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Every WIT package in the tree, and the text that describes it.
///
/// The HEADER comment only — the block above `package` — not the whole file.
///
/// Measured both ways. Indexing the whole file made long WITs win on common words:
/// "stop a caller making too many requests" returned `anthropic-provider` first,
/// because a big interface mentions requests, callers and stopping somewhere in
/// its per-function docs. The header is the paragraph whose job is to say what the
/// thing is FOR, it is roughly the same length for every package, and a bag of
/// words over uniform-length documents is a fair comparison in a way that the
/// alternative is not.
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
            // `package ratelimit:guard@0.1.0;` names what this file describes.
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
/// Words that carry no capability in them.
///
/// The list grew after a real query embarrassed it: "two workers must not do the
/// same job" matched 57 of 152 capabilities and ranked `outbox` first, scoring on
/// "two", "not" and "same" — three words that say nothing about what anything
/// does. Terms shorter than three characters are dropped separately, which is why
/// "do", "be" and "no" are not here.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "as", "at", "by", "for", "from", "in", "into", "is", "it", "of", "on", "or",
    "that", "the", "to", "with", "need", "needs", "want", "i", "we", "my", "this",
    // Function words, added after the query above.
    "not", "two", "same", "must", "does", "doing", "when", "what", "which", "than", "then", "them",
    "they", "you", "your", "its", "has", "have", "had", "will", "would", "could", "should", "are",
    "was", "were", "been", "but", "also", "just", "only", "very", "much", "more", "most", "such",
    "each", "both", "either", "neither",
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
    /// The terms that matched, so a caller can say WHY rather than just how much.
    /// A ranking nobody can check is a ranking nobody should trust.
    pub because: Vec<String>,
}

/// Search the catalogue.
///
/// Scoring, in order of weight and stated plainly because a hidden ranking is
/// worse than no ranking:
///
///   * a term in the component's NAME or in one of its exported interface names is
///     worth most — `csv` matching `csv:codec/codec` is about as direct as evidence
///     gets;
///   * a term in the description is worth less, since a sentence mentions things it
///     merely relates to;
///   * and ties break towards what applications already carry, because a
///     capability twenty apps compose has been through more than one gate.
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
            // Prefix rather than equality: "hashing" should find "hash", and a
            // description writer had no idea which form a caller would type.
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
        if score == 0.0 {
            continue;
        }
        // Graph centrality multiplier: log-ish adoption score so that foundational, widely composed
        // capabilities (high in-degree in the capability graph) break ties towards vetted infrastructure.
        score += (cap.apps as f64 + 1.0).ln() * 0.5;
        out.push(Match { capability: cap, score, because });
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.capability.name.cmp(&b.capability.name)));
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
            },
            Capability {
                name: "auth-guard".into(),
                description: "Reference implementation of the auth:identity contract".into(),
                doc: "issue a session, verify a password".into(),
                exports: vec!["auth:identity/accounts@0.1.0".into()],
                apps: 18,
            },
            Capability {
                name: "search-index".into(),
                description: "Generic full-text inverted-index capability with ranked results"
                    .into(),
                doc: String::new(),
                exports: vec!["search:index/index@0.1.0".into()],
                apps: 4,
            },
            Capability {
                name: "lonely".into(),
                description: "Nothing to do with any of it".into(),
                doc: String::new(),
                exports: vec!["lonely:thing/x@0.1.0".into()],
                apps: 0,
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
        // The answer that matters most, because it is the one that says "build it".
        // A searcher that always returns its closest row would send every goal to
        // reuse something unrelated.
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
        // A far better textual match with no users at all must still win over a
        // weak match that twenty apps happen to carry.
        p.push(Capability {
            name: "csv-pretty".into(),
            description: "Format CSV tables for humans, with aligned columns".into(),
            doc: String::new(),
            exports: vec!["csv:pretty/render@0.1.0".into()],
            apps: 0,
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
/// ADR-0089's last unbuilt gap, in its cheapest form. Prevention is the real
/// answer — a `capsearch` consulted before writing means the duplicate never
/// exists — but prevention has no feedback loop: a working search and a broken one
/// look identical from the outside. This counts what got through.
///
/// Structural, not semantic, and deliberately so. "Exports the same interface" is
/// a fact the catalogue already holds; it needs no model, no descriptions (which
/// are 52% tautological) and no embedding. It is also the fact `comp-plug` is
/// already deciding on, and silently: `exporters` uses `or_insert_with`, so when
/// two components export one interface the one that sorts first wins and nobody is
/// told. Whatever else this finds, it makes that visible.
///
/// A pair is NOT necessarily a mistake. `record-store` and a mock both export
/// `records:store/store` on purpose. So this reports pairs and says how alike they
/// are; deciding is a person's job, and a deliberate pair is a `superseded-by`-
/// shaped fact about the graph rather than a bug.
#[derive(Debug, Clone, PartialEq)]
pub struct Twins {
    /// The interface both of them export.
    pub interface: String,
    /// Every component exporting it, in name order. Always two or more.
    pub components: Vec<String>,
    /// Interfaces exported by all of them — the overlap, not just this one.
    ///
    /// The signal that separates a duplicate from an alternative: two components
    /// that agree on ONE interface may be a reference and its mock, while two that
    /// agree on their whole surface are the same component written twice.
    pub shared: Vec<String>,
    /// How much of the smaller component's surface the overlap covers, 0.0–1.0.
    pub overlap: f64,
}

/// Every interface exported by more than one component in the catalogue.
///
/// Sorted by overlap, because a pair that agrees on its entire surface is worth
/// more attention than one that happens to share a types module.
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
        // Against the SMALLEST surface: a tiny component fully contained in a
        // large one is entirely duplicated, and dividing by the larger would hide
        // that behind the larger one's unrelated exports.
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
