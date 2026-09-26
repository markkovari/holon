//! Every contract in the repository, read from both sides.
//!
//! A WIT interface has two halves that are written in different crates, months
//! apart, and nothing checks that they still agree. The compiler cannot: a
//! component compiles perfectly while importing an interface no component on earth
//! exports, and the failure shows up at composition — or later, at deployment, as
//! an artifact that still carries the import.
//!
//! So this reads the whole catalogue and asks two questions:
//!
//!   * **consumer side** — does every import have a provider? An import nothing
//!     satisfies is a composition that can never be complete.
//!   * **provider side** — does every export have a consumer? An export nobody
//!     imports is not a bug, but it is worth being able to see: it is either a
//!     capability waiting for its first user, or one whose users have moved on.
//!
//! The first is a failure. The second is a REPORT, printed and never asserted,
//! because a capability library is allowed to be ahead of its callers — this repo
//! is largely a catalogue of capabilities and roughly half of them are unused by
//! anything else in-tree. Turning that into a test would be a test of fashion.
//!
//! Both read the built artifacts, not the WIT sources: what a component actually
//! imports is what survived compilation, and `components/*/wit/` can say things
//! the binary does not (see `plug::Surface`).

use std::collections::BTreeMap;

use comp_reconciler::fleet::repo_root;
use comp_reconciler::plug::{default_dirs, Catalog};

fn catalogue() -> Option<Catalog> {
    let cat = Catalog::scan(&default_dirs(&repo_root()));
    if cat.is_empty() {
        eprintln!(
            "SKIPPED: nothing is built, so no contract was checked by this run. \
             `cargo xtask build --force` first."
        );
        return None;
    }
    Some(cat)
}

#[test]
fn every_import_has_a_provider() {
    let Some(cat) = catalogue() else { return };

    let mut orphans: Vec<String> = Vec::new();
    for name in cat.names().map(String::from).collect::<Vec<_>>() {
        for iface in cat.unmet(&name) {
            orphans.push(format!("  {name} imports {iface}, which nothing exports"));
        }
    }
    orphans.sort();

    assert!(
        orphans.is_empty(),
        "these components can never be composed completely:\n{}\n\n\
         Either the provider was deleted, or the versions drifted apart — an import \
         of `foo:bar/baz@0.1.0` is NOT satisfied by an export of `foo:bar/baz@0.2.0`, \
         and `wac` will not tell you, it will just leave the import in place.",
        orphans.join("\n")
    );
    println!("  {} components, every import has a provider", cat.len());
}

/// And every one of them actually ENCODES.
///
/// A provider existing is not the same as a composition being expressible.
/// `agent-driver` had a provider for every import and could not be composed at all:
/// the interface it exports borrowed its types from the interfaces it imports, so
/// satisfying one made the export reference an instance that had become internal,
/// and the encoder refused with "instance not valid to be used as export".
///
/// `driver-probe` — the loop driver's entire HTTP surface — was unbuildable for as
/// long as that was true, and nothing looked. `every_import_has_a_provider` was
/// green the whole time, because it is: it asks a different question.
#[test]
fn every_composition_encodes() {
    let Some(cat) = catalogue() else { return };
    let out = repo_root().join("components/target/composed");

    let mut broken: Vec<String> = Vec::new();
    let mut composed = 0usize;
    for name in cat.names().map(String::from).collect::<Vec<_>>() {
        // Only what CAN be composed completely. An unmet import is the other
        // test's subject, and reporting it here too would say one thing twice.
        if !cat.unmet(&name).is_empty() {
            continue;
        }
        // Nothing to join is not a composition.
        let Ok(wiring) = comp_reconciler::plug::wiring(&name, &cat) else { continue };
        if wiring.plugs.is_empty() {
            continue;
        }
        composed += 1;
        if let Err(e) = comp_reconciler::plug::compose_to(&name, &cat, &out) {
            broken.push(format!("  {name}: {e}"));
        }
    }
    broken.sort();

    assert!(
        broken.is_empty(),
        "these components have every provider they need and STILL do not compose:\n{}\n\n\
         The usual cause is an exported interface that `use`s types from an imported \
         one — satisfying the import makes that instance internal, and an export \
         cannot reference it. Give the exported interface its own types and convert \
         at the edge (see components/graph-run/wit/run.wit).",
        broken.join("\n")
    );
    println!("  {composed} compositions encode");
}

/// The provider side: who is actually using what.
///
/// Printed rather than asserted. Its value is the shape of the list, not a pass:
/// a capability with many consumers is load-bearing and its interface should be
/// treated as frozen, while one with none can still be changed freely.
#[test]
fn report_who_consumes_each_capability() {
    let Some(cat) = catalogue() else { return };

    let names: Vec<String> = cat.names().map(String::from).collect();
    let mut consumers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in &names {
        let Some(surface) = cat.surface(name) else { continue };
        for export in &surface.exports {
            consumers.entry(export.clone()).or_default();
        }
    }
    for name in &names {
        let Some(surface) = cat.surface(name) else { continue };
        for import in &surface.imports {
            if let Some(list) = consumers.get_mut(import) {
                list.push(name.clone());
            }
        }
    }

    let used: Vec<_> = consumers.iter().filter(|(_, c)| !c.is_empty()).collect();
    let unused: Vec<_> = consumers.iter().filter(|(_, c)| c.is_empty()).collect();

    println!(
        "\n  {} interfaces exported, {} of them consumed in-tree\n",
        consumers.len(),
        used.len()
    );
    println!("  load-bearing (interface: consumers)");
    let mut ranked: Vec<_> = used.iter().map(|(i, c)| (c.len(), i, c)).collect();
    ranked.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (n, iface, who) in ranked.iter().take(12) {
        println!("    {n:>2}  {iface}  ({})", who.join(", "));
    }
    println!(
        "\n  exported but unconsumed in-tree: {} — a capability catalogue is allowed \n  to be ahead of its callers, so this is a fact, not a finding.",
        unused.len()
    );
}

/// A version of an interface that only one side moved to.
///
/// The failure mode `every_import_has_a_provider` reports as "nothing exports it",
/// but the cause deserves its own name: somebody bumped a package and left the
/// other half behind. Reported separately so the message says which.
#[test]
fn report_interfaces_that_exist_at_two_versions() {
    let Some(cat) = catalogue() else { return };

    let mut versions: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in cat.names().map(String::from).collect::<Vec<_>>() {
        let Some(surface) = cat.surface(&name) else { continue };
        for iface in surface.exports.iter().chain(surface.imports.iter()) {
            let Some((base, version)) = iface.rsplit_once('@') else { continue };
            let entry = versions.entry(base.to_string()).or_default();
            if !entry.contains(&version.to_string()) {
                entry.push(version.to_string());
            }
        }
    }
    let split: Vec<_> = versions.iter().filter(|(_, v)| v.len() > 1).collect();
    if split.is_empty() {
        println!("  no interface exists at two versions");
        return;
    }
    println!("\n  interfaces present at more than one version:");
    for (iface, vs) in &split {
        println!("    {iface} @ {}", vs.join(", "));
    }
}
