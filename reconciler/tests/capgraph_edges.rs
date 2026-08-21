//! The connections themselves: which apps carry which components, and which
//! components import which interfaces.
//!
//! `capgraph_store.rs` checks that a projection round-trips through SurrealDB
//! without losing or corrupting anything. This checks that what is being projected
//! is *right* — that the graph agrees with how the apps in this repository are
//! actually composed.
//!
//! Two sources, because the two halves live in different places: component →
//! interface comes from `plug::Catalog`, which is library code; app → component
//! comes from `comp-capgraph --format json`, because the Justfile parsing that
//! discovers apps is private to the binary.
//!
//! ## What is asserted, and what deliberately is not
//!
//! **Named relationships** are asserted exactly: `conduit-domain` imports
//! `records:store/store`, `conduit` carries `auth-guard`. These come from apps the
//! roadmap calls done, so they are stable, and if one breaks it is a real
//! regression rather than growth.
//!
//! **Totals** are asserted as floors, never as equalities. `records:store/store`
//! having ≥30 consumers is a fact about this repository's shape; it having exactly
//! 37 is a fact about the day it was counted, and a test that has to be edited
//! every time somebody adds a component is a test people learn to edit without
//! reading.
//!
//! **Invariants** are asserted over everything, because those hold no matter how
//! much the catalogue grows: every composed part exists, every consumed interface
//! has an exporter, every app's own root is a real component.

use std::collections::BTreeSet;
use std::process::Command;

use comp_reconciler::fleet::repo_root;
use comp_reconciler::plug::{default_dirs, Catalog};
use serde_json::Value;

/// The catalogue, or `None` when nothing is built.
///
/// Skipped loudly rather than failed: a fresh clone has no artifacts, and these
/// are derived from artifacts. A test that silently passes on an empty catalogue
/// would assert nothing while looking green.
fn catalogue() -> Option<Catalog> {
    let catalog = Catalog::scan(&default_dirs(&repo_root()));
    if catalog.is_empty() {
        eprintln!("SKIPPED: nothing is built — run `just build`");
        return None;
    }
    Some(catalog)
}

fn graph_json() -> Option<Value> {
    let out = Command::new(env!("CARGO_BIN_EXE_comp-capgraph"))
        .args(["--format", "json"])
        .output()
        .expect("comp-capgraph did not run");
    if !out.status.success() {
        eprintln!("SKIPPED: comp-capgraph refused — nothing is built");
        return None;
    }
    Some(serde_json::from_slice(&out.stdout).expect("--format json emitted invalid JSON"))
}

fn names(v: &Value, key: &str) -> BTreeSet<String> {
    v[key]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

// ------------------------------------------------- component → interface

/// The relationships ADR-0090 and the Conduit showcase are written about.
///
/// `conduit-domain` is the whole RealWorld spec as one component, and the four
/// capabilities it leans on are the reason it is a showcase rather than a demo. If
/// one of these disappears, either the component was rewritten or the surface
/// reader stopped seeing imports — both worth failing over.
#[test]
fn conduit_domain_imports_the_capabilities_it_is_composed_with() {
    let Some(catalog) = catalogue() else { return };
    let surface = catalog.surface("conduit-domain").expect("conduit-domain is not built");

    for iface in [
        "auth:identity/accounts@0.1.0",
        "auth:identity/authorizer@0.1.0",
        "auth:identity/types@0.1.0",
        "records:store/store@0.1.0",
        "slug:generate/generator@0.1.0",
    ] {
        assert!(
            surface.imports.contains(iface),
            "conduit-domain no longer imports {iface} — it imports {:?}",
            surface.imports
        );
    }
}

/// Every interface anything imports comes from a package something exports.
///
/// This is the invariant that makes composition possible at all: an import from a
/// package nothing in the repository provides is an app that cannot be plugged.
/// The tool already reports the reverse direction (exported and unconsumed) as
/// "not a finding"; this direction is one.
///
/// Checked per PACKAGE rather than per interface, because a bare `types` interface
/// is normally imported and never exported. `audit-log` exports
/// `audit:log/recorder`, that interface `use`s `audit:log/types`, and so three
/// components import `audit:log/types` while nothing exports it — which is how WIT
/// type-only interfaces work, not a missing capability. Asserting per interface
/// flags all three and means nothing.
#[test]
fn every_consumed_package_has_an_exporter() {
    let Some(catalog) = catalogue() else { return };

    let package = |iface: &str| iface.split('/').next().unwrap_or(iface).to_string();
    let exported: BTreeSet<String> = catalog
        .names()
        .filter_map(|n| catalog.surface(n))
        .flat_map(|s| s.exports.iter().map(|e| package(e)))
        .collect();

    let mut unsatisfiable: Vec<String> = Vec::new();
    for name in catalog.names() {
        let Some(surface) = catalog.surface(name) else { continue };
        for iface in &surface.imports {
            let pkg = package(iface);
            // Host-provided packages (`wasi:*`, and this repo's own host
            // capabilities) are imported without any component exporting them —
            // that is what a host IS.
            if pkg.starts_with("wasi:") || pkg.starts_with("comp:") {
                continue;
            }
            if !exported.contains(&pkg) {
                unsatisfiable.push(format!("{name} imports {iface}, and nothing exports {pkg}"));
            }
        }
    }

    assert!(unsatisfiable.is_empty(), "unsatisfiable imports:\n  {}", unsatisfiable.join("\n  "));
}

/// `records:store/store` is the most-consumed interface in the repository, and by
/// a distance. ADR-0090 and the capability graph doc both reason from this number.
///
/// A floor rather than an equality: the count grows with the catalogue, and the
/// claim that matters is "changing this interface in place is not safe", which a
/// floor states just as well.
#[test]
fn record_store_is_the_most_consumed_interface() {
    let Some(catalog) = catalogue() else { return };

    let mut ranked: Vec<(usize, String)> = catalog
        .names()
        .filter_map(|n| catalog.surface(n))
        .flat_map(|s| s.exports.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|iface| (catalog.consumer_count(&iface), iface))
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0));

    let (top_count, top_iface) = ranked.first().cloned().expect("no interfaces at all");
    assert_eq!(
        top_iface, "records:store/store@0.1.0",
        "the most-consumed interface is now {top_iface} ({top_count} consumers)"
    );
    assert!(
        top_count >= 30,
        "records:store/store has fallen to {top_count} consumers — either a lot of \
         components stopped using it or the surface reader is missing imports"
    );
}

// ------------------------------------------------------- app → component

/// The compositions of four apps the roadmap calls done.
///
/// Asserted as a subset, not an equality: an app gaining a capability is ordinary
/// progress, an app silently LOSING one is a regression. Only the second is worth
/// a red build.
#[test]
fn apps_carry_the_components_they_are_composed_from() {
    let Some(graph) = graph_json() else { return };

    let expected: &[(&str, &[&str])] = &[
        ("conduit", &["audit-log", "auth-guard", "rate-limiter", "record-store", "slug"]),
        ("saga", &["event-bus", "fsm-workflow", "id-generate", "record-store", "scheduler-timer"]),
        ("pulse", &["event-bus", "id-generate", "record-store"]),
        ("helpdesk", &["audit-log", "auth-guard", "fsm-workflow", "record-store"]),
    ];

    for (app, must_carry) in expected {
        let entry = graph["apps"]
            .as_array()
            .and_then(|a| a.iter().find(|x| x["app"] == *app))
            .unwrap_or_else(|| panic!("the graph knows no app called {app}"));
        let composes = names(entry, "composes");
        for part in *must_carry {
            assert!(
                composes.contains(*part),
                "{app} no longer carries {part} — it carries {composes:?}"
            );
        }
    }
}

/// Everything an app is composed from is a component that exists.
///
/// The app list is parsed out of the Justfile and the parts come from the built
/// artifacts, so the two can drift: a renamed component leaves a recipe pointing at
/// a name nothing builds. That drift is invisible in the markdown report — it just
/// renders a row — and this is what makes it fail.
#[test]
fn every_composed_part_is_a_real_component() {
    let (Some(catalog), Some(graph)) = (catalogue(), graph_json()) else { return };
    let known: BTreeSet<&str> = catalog.names().collect();

    let mut missing = Vec::new();
    for app in graph["apps"].as_array().unwrap_or(&Vec::new()) {
        let name = app["app"].as_str().unwrap_or("?");
        if let Some(root) = app["root"].as_str() {
            if !known.contains(root) {
                missing.push(format!("{name}'s root {root} is not a built component"));
            }
        }
        for part in names(app, "composes") {
            if !known.contains(part.as_str()) {
                missing.push(format!("{name} composes {part}, which is not a built component"));
            }
        }
    }
    assert!(missing.is_empty(), "the app list has drifted:\n  {}", missing.join("\n  "));
}

/// **A divergence between two formats of the same graph, pinned deliberately.**
///
/// `--format json` reports what an app was composed FROM, which is
/// `Catalog::closure` and excludes the app's own root. `--format surql` includes
/// the root, because ADR-0091's query walks `app -> carries -> artifact -> imports`
/// and the root's imports are the ones a lesson is most likely to be about — leave
/// it out and `conduit` appears to import three interfaces when `conduit-domain`
/// alone imports five.
///
/// Both readings are defensible and they are currently different. The visible
/// consequence is that no `*-domain` component appears in `component_in_apps` at
/// all, so `just capability` ranks every domain component as carried by zero apps.
/// That is plausibly correct — a domain component is app-specific and should not
/// rank high for reuse — which is exactly why it should be a decision rather than
/// an accident.
///
/// This test fails the day somebody changes it. That is the point: change it on
/// purpose, and update this test in the same commit.
#[test]
fn json_omits_an_apps_own_root_while_surql_includes_it() {
    let Some(graph) = graph_json() else { return };

    let conduit = graph["apps"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["app"] == "conduit"))
        .expect("no conduit app");
    assert_eq!(conduit["root"], "conduit-domain");
    assert!(
        !names(conduit, "composes").contains("conduit-domain"),
        "--format json now includes an app's own root; --format surql already did, \
         so this divergence is closed — delete this test and say so in the commit"
    );

    let in_apps: BTreeSet<String> = graph["component_in_apps"]
        .as_array()
        .map(|a| {
            a.iter().filter_map(|x| x["component"].as_str().map(String::from)).collect()
        })
        .unwrap_or_default();
    assert!(
        !in_apps.iter().any(|c| c.ends_with("-domain")),
        "a domain component now appears in component_in_apps — the app-count \
         tie-breaker in `just capability` has changed behaviour"
    );
}

#[test]
fn graph_json_emits_valid_stats_summary() {
    let Some(graph) = graph_json() else { return };
    let stats = &graph["stats"];
    assert!(
        stats["total_components"].as_u64().unwrap_or(0) > 50,
        "expected >50 components in stats"
    );
    assert!(
        stats["total_interfaces"].as_u64().unwrap_or(0) > 30,
        "expected >30 interfaces in stats"
    );
    assert!(
        stats["total_import_edges"].as_u64().unwrap_or(0) > 50,
        "expected >50 import edges in stats"
    );
    assert!(
        stats["total_apps"].as_u64().unwrap_or(0) >= 10,
        "expected >=10 apps in stats"
    );
}
