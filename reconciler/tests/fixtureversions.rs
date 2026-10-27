//! A fixture link names a version that something actually exports.
//!
//! `wit/SURFACES.md` (see `witsurface.rs`) catches a SHAPE that moved without its version; this catches a
//! version that moved and left every consumer behind. `wac plug` matches on the WHOLE versioned string,
//! so thirteen `links:` entries across seven fixtures asking for 0.1.0 after `graph:fitness/evaluator` and
//! `graph:run/driver` went to 0.2.0 silently resolved to nothing, and the agent loop died as
//!
//!     goalrun.acme.test never served within 180s - last: HTTP 503
//!
//! naming no interface, component or version. `fixtures.rs` asks whether a fixture PARSES and its ids
//! RESOLVE, a different kind of thing from a YAML version against a built artifact.
//!
//! Deliberately narrow: only that some component exports the version being asked for. An import naming a
//! package this repository does not build is skipped (a host satisfies it; see `hostsurface.rs`).

use std::collections::{BTreeMap, BTreeSet};

use holon_fleet::fleet::repo_root;
use holon_fleet::plug::Catalog;

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
        "only {} components built — run `cargo xtask build --force` first",
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
