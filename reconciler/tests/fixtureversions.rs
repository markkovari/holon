//! A fixture link names a version that something actually exports.
//!
//! `wit/SURFACES.md` (see `witsurface.rs`) catches a SHAPE that moved without its
//! version. This catches the other half of the same mistake: a version that moved
//! and left every consumer behind.
//!
//! Both happened, one merge apart, and neither was noticed. `graph:fitness/evaluator`
//! went to 0.2.0 in #147 and `graph:run/driver` in #148; thirteen `links:` entries
//! across seven fixtures kept asking for 0.1.0. `wac plug` matches an import to an
//! export on the WHOLE versioned string, so every one of those links silently
//! resolved to nothing — and the entire agent loop could not start. A run died as
//!
//!     goalrun.acme.test never served within 180s — last: HTTP 503
//!
//! which names no interface, no component and no version.
//!
//! `fixtures.rs` passed throughout, and correctly: it asks whether a fixture PARSES
//! and whether its ids RESOLVE against each other. Nothing compared a version in a
//! YAML file against a version in a built artifact, because those are different
//! kinds of thing living in different directories — which is exactly why it needed
//! a test of its own rather than an extra assertion in that one.
//!
//! Deliberately narrow. It says nothing about whether the interface is the RIGHT
//! one, only that some component in the tree exports the version being asked for.
//! An import naming a package this repository does not build at all is skipped: a
//! host satisfies those, and they are `hostsurface.rs`'s business.

use std::collections::{BTreeMap, BTreeSet};

use comp_reconciler::fleet::repo_root;
use comp_reconciler::plug::Catalog;

/// `graph:run/driver@0.2.0` -> `graph:run/driver`.
fn base(iface: &str) -> &str {
    iface.split('@').next().unwrap_or(iface)
}

#[test]
fn every_fixture_link_names_a_version_something_exports() {
    let root = repo_root();
    let catalog = Catalog::scan(&[root.join("components/target/wasm32-wasip2/release")]);
    assert!(
        catalog.names().count() > 100,
        "only {} components built — run `just build` first",
        catalog.names().count()
    );

    // interface-without-version -> every version of it that is exported.
    let mut exported: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in catalog.names().map(str::to_string).collect::<Vec<_>>() {
        let Some(surface) = catalog.surface(&name) else { continue };
        for export in &surface.exports {
            exported.entry(base(export).to_string()).or_default().insert(export.clone());
        }
    }

    let dir = root.join("fixtures");
    let mut wrong = Vec::new();
    let mut checked = 0usize;
    let mut files = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
        .collect::<Vec<_>>();
    files.sort();
    assert!(!files.is_empty(), "no fixtures in {}", dir.display());

    for path in &files {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        for line in text.lines() {
            let Some(rest) = line.trim().strip_prefix("import:") else { continue };
            let want = rest.trim();
            // Only versioned, package-qualified interfaces.
            if !want.contains('@') || !want.contains(':') || !want.contains('/') {
                continue;
            }
            let Some(have) = exported.get(base(want)) else { continue }; // host-provided
            checked += 1;
            if !have.contains(want) {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                wrong.push(format!(
                    "  {name} links on `{want}`\n    but the built artifact exports {}",
                    have.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(", ")
                ));
            }
        }
    }

    assert!(
        wrong.is_empty(),
        "fixture(s) link on an interface version nothing exports.\n\n\
         `wac plug` matches on the whole versioned string, so these links resolve to \n\
         NOTHING and the app will not serve — as an HTTP 503 that names none of this.\n\
         Bump the fixture to the version the component now exports.\n\n{}",
        wrong.join("\n")
    );

    println!("  {} link(s) across {} fixture(s), every version exported", checked, files.len());
}
