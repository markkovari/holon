//! "Do we already have this?" without a model: term overlap, ties broken by the graph (ADR-0089).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use comp_fleet::plug::Catalog;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capability {
    pub name: String,
    /// From the component's own `//!` line; often a tautology, which is why `doc` exists.
    pub description: String,
    /// The component's WIT header as prose, derived so it cannot drift from the contract.
    pub doc: String,
    pub exports: Vec<String>,
    /// Ranked below every matching capability, not dropped: it still proves a composition exists.
    pub is_app: bool,
    /// How many apps carry it: the tie-breaker, as a widely composed capability is safer.
    pub apps: usize,
}

/// Applies `crate::catalogue`'s rules directly rather than reading the generated `catalog.json`.
pub fn capabilities(
    repo_root: &Path,
    catalog: &Catalog,
    apps_of: &BTreeMap<String, usize>,
) -> Vec<Capability> {
    // A component exporting nothing outside `wasi:` offers no contract, so nothing can plug it.
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
        if surface.exports.is_empty() {
            continue;
        }
        // Prose belongs to the package: `auth-guard` has no wit directory, so match by export.
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

/// The header only: whole files made long WITs win on common words.
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

/// Grown from real false hits ("two", "not", "same"); terms under three characters drop separately.
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

#[derive(Debug, Clone)]
pub struct Match<'a> {
    pub capability: &'a Capability,
    pub score: f64,
    /// A ranking nobody can check is a ranking nobody should trust.
    pub because: Vec<String>,
}

/// Name or exported-interface terms weigh most, description terms less; ties go to what apps carry.
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
            // Prefix, not equality: "hashing" should find "hash".
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
        // One shared word is a coincidence ("imagery" matched `image-optimizer`).
        if score == 0.0 || (wanted.len() > 1 && because.len() < 2) {
            continue;
        }
        // Log-ish adoption breaks ties towards vetted infrastructure.
        score += (cap.apps as f64 + 1.0).ln() * 0.5;
        out.push(Match { capability: cap, score, because });
    }
    // A showcase never outranks the thing it is built from.
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
        // "Build it" matters most: always returning the closest row sends every goal to reuse.
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
        // A far better match with no users must still beat a weak one twenty apps carry.
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

/// Structural, and what `comp-plug` decides silently; a pair is not necessarily a mistake (mocks).
#[derive(Debug, Clone, PartialEq)]
pub struct Twins {
    pub interface: String,
    pub components: Vec<String>,
    /// The overlap separates an alternative (a mock) from the same component written twice.
    pub shared: Vec<String>,
    pub overlap: f64,
}

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
        // Against the smallest surface, or a tiny component fully inside a large one looks unique.
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
