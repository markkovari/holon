//! Every showcase UI still installs from a clean checkout.
//!
//! Eight of them did not. `@vitejs/plugin-react@^4` was pinned against
//! `vite@^8`, plugin-react 4 peers on `vite@^4 || ^5 || ^6 || ^7`, and `npm ci`
//! refuses that outright — so `just build-books-ui` and seven siblings failed on
//! any machine that did not already have a populated `node_modules`. Every
//! developer who had built once kept working; nobody starting fresh could.
//!
//! That is the shape worth guarding: a break that is invisible to everyone who
//! could fix it. The npm dependency bot upgrades `vite` on its own schedule and
//! has no opinion about the plugin's peer range, so this will be attempted again.
//!
//! ## Why the lockfile and not `npm ci`
//!
//! `npm ci --dry-run` is the real check and it costs a network round trip per UI,
//! fifteen of them, which is a test that gets skipped. The lockfile already
//! records both the resolved version of every package and the peer ranges it
//! declares — the same two facts npm compares — so the check runs offline in
//! milliseconds against exactly what a fresh install would resolve.
//!
//! Deliberately partial: only caret ranges and only majors, because that is what
//! the ecosystem actually writes and what the failure looked like. Anything this
//! cannot parse is skipped rather than guessed at — a linter that invents a
//! verdict on syntax it does not understand gets turned off.

use std::path::PathBuf;

use serde_json::Value;

use comp_reconciler::fleet::repo_root;

/// Every `examples/*/ui/package-lock.json`.
fn lockfiles() -> Vec<PathBuf> {
    let examples = repo_root().join("examples");
    let Ok(entries) = std::fs::read_dir(&examples) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path().join("ui/package-lock.json"))
        .filter(|p| p.is_file())
        .collect();
    out.sort();
    out
}

/// The majors a `^a.b.c || ^x.y.z` range admits, or `None` if this is not a shape
/// we claim to understand.
///
/// `^0.x` is deliberately excluded: under semver a caret on a zero major pins the
/// MINOR, so treating it as "major 0 is fine" would wave through a real conflict.
fn caret_majors(range: &str) -> Option<Vec<u64>> {
    let mut majors = Vec::new();
    for part in range.split("||") {
        let part = part.trim();
        let rest = part.strip_prefix('^')?;
        let major: u64 = rest.split('.').next()?.parse().ok()?;
        if major == 0 {
            return None;
        }
        majors.push(major);
    }
    (!majors.is_empty()).then_some(majors)
}

#[test]
fn every_showcase_ui_installs_from_a_clean_checkout() {
    let mut conflicts: Vec<String> = Vec::new();
    let mut checked = 0usize;
    let files = lockfiles();
    assert!(!files.is_empty(), "no showcase UI lockfiles found — has examples/ moved?");

    for lock in &files {
        let Ok(text) = std::fs::read_to_string(lock) else { continue };
        let Ok(doc) = serde_json::from_str::<Value>(&text) else { continue };
        let Some(packages) = doc["packages"].as_object() else { continue };
        let ui = lock.parent().and_then(|p| p.parent()).and_then(|p| p.file_name());
        let ui = ui.map(|s| s.to_string_lossy().to_string()).unwrap_or_default();

        for (path, entry) in packages {
            let Some(peers) = entry["peerDependencies"].as_object() else { continue };
            let name = path.rsplit("node_modules/").next().unwrap_or(path);
            for (peer, range) in peers {
                // An optional peer that is absent is not a conflict; that is what
                // optional means.
                if entry["peerDependenciesMeta"][peer]["optional"] == Value::Bool(true) {
                    continue;
                }
                let Some(installed) = packages
                    .get(&format!("node_modules/{peer}"))
                    .and_then(|p| p["version"].as_str())
                else {
                    continue; // not installed at all — npm's problem, not this check's
                };
                let Some(range) = range.as_str().and_then(caret_majors) else { continue };
                let Some(have): Option<u64> = installed.split('.').next().and_then(|m| m.parse().ok())
                else {
                    continue;
                };
                checked += 1;
                if !range.contains(&have) {
                    conflicts.push(format!(
                        "  examples/{ui}/ui: {name}@{} peers on {peer} \"{}\" but {peer}@{installed} is locked",
                        entry["version"].as_str().unwrap_or("?"),
                        range
                            .iter()
                            .map(|m| format!("^{m}"))
                            .collect::<Vec<_>>()
                            .join(" || "),
                    ));
                }
            }
        }
    }

    assert!(
        conflicts.is_empty(),
        "{} peer conflict(s) — `npm ci` fails on a clean checkout for these ({checked} peers \
         checked across {} UIs):\n{}",
        conflicts.len(),
        files.len(),
        conflicts.join("\n")
    );
    println!("  {checked} peer ranges across {} showcase UIs, all satisfied", files.len());
}
