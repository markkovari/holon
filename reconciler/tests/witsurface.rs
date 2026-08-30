//! A version is a claim about a SHAPE, and this is what checks it.
//!
//! An interface id carries its version — `graph:fitness/evaluator@0.2.0` — and
//! `wac plug` matches an import to an export on that whole string. So the version
//! is the only thing standing between "these two components fit" and a failure
//! that reads:
//!
//!     the socket component had no matching imports for the plugs that were provided
//!
//! which names neither the interface nor the reason. Measured against this
//! repository's own components rather than assumed:
//!
//!   - adding a FUNCTION to an interface keeps a stale consumer pluggable;
//!   - adding a case to a VARIANT does not;
//!   - adding a field to a RECORD does not.
//!
//! So most interesting changes to a data-carrying interface are breaking, and the
//! version has to move with them. `graph:fitness` shipped one merge where it did
//! not, and nothing caught it — one version string meaning two different shapes.
//!
//! ## What is snapshotted, and why from the artifact
//!
//! `wit/SURFACES.md` holds every package this repository defines, as
//! `wasm-tools component wit` renders it out of the BUILT COMPONENT. That is the
//! shape that actually shipped, it is canonical, and it carries no doc comments —
//! so editing a comment does not churn the file and changing a type does.
//!
//! The file is committed because the DIFF is the review. A pull request that
//! changes a shape shows the reader exactly what moved, next to the version that
//! did or did not move with it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use comp_reconciler::fleet::repo_root;

/// Namespaces this repository does not define. `root` is the anonymous world every
/// component carries; `wasi` is upstream's and moves on its own schedule.
const FOREIGN: &[&str] = &["wasi", "root"];

fn artifacts(root: &Path) -> Vec<PathBuf> {
    let dir = root.join("components/target/wasm32-wasip2/release");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|d| {
            d.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "wasm"))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// The INTERFACES in one component's rendered WIT, ours only, keyed by the id
/// `wac plug` actually matches on: `ns:pkg/iface@version`.
///
/// Interfaces rather than packages, because a component renders only the parts of a
/// package it uses — `binder-domain` prints three interfaces of `auth:identity`,
/// `auth-guard` prints five.
///
/// And only the interfaces a component EXPORTS are its shape. A consumer renders
/// only the FUNCTIONS it imports: `arena-domain` prints `id:generate/generator` as
/// one function, `nanoid`, while the provider prints five. That is not staleness,
/// it is the component model's subtyping made visible — and it is exactly why
/// adding a function to an interface keeps an old consumer pluggable. So the shape
/// worth versioning is the PROVIDER's, and a consumer's is a requirement rather
/// than a definition.
fn interfaces_in(text: &str) -> BTreeMap<String, String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = BTreeMap::new();
    let mut i = 0usize;
    while i < lines.len() {
        let Some(pkg) = lines[i].strip_prefix("package ").and_then(|r| r.strip_suffix(" {")) else {
            i += 1;
            continue;
        };
        if FOREIGN.iter().any(|f| pkg.starts_with(&format!("{f}:")) || pkg == *f) {
            i += 1;
            continue;
        }
        let (name, version) = pkg.split_once('@').unwrap_or((pkg, ""));
        i += 1;

        // To the package's closing brace at column 0, picking up each interface.
        while i < lines.len() && lines[i] != "}" {
            let Some(iface) =
                lines[i].trim().strip_prefix("interface ").and_then(|r| r.strip_suffix(" {"))
            else {
                i += 1;
                continue;
            };
            let mut body = vec![lines[i].trim_end().to_string()];
            let mut depth = 1usize;
            i += 1;
            while i < lines.len() && depth > 0 {
                body.push(lines[i].trim_end().to_string());
                depth += lines[i].matches('{').count();
                depth -= lines[i].matches('}').count();
                i += 1;
            }
            let id = if version.is_empty() {
                format!("{name}/{iface}")
            } else {
                format!("{name}/{iface}@{version}")
            };
            out.insert(id, body.join("\n"));
        }
    }
    out
}

/// Every interface this repository defines, as its built components render it.
fn surfaces(root: &Path) -> Option<BTreeMap<String, String>> {
    let mut all: BTreeMap<String, String> = BTreeMap::new();
    let mut disagreements: Vec<String> = Vec::new();
    let files = artifacts(root);
    if files.is_empty() {
        eprintln!("SKIPPED: nothing is built — run `just build`");
        return None;
    }
    for f in &files {
        // What this component EXPORTS — the interfaces it is the definition of.
        let Ok(bytes) = std::fs::read(f) else { continue };
        let Ok(surface) = comp_reconciler::plug::surface(&bytes) else { continue };
        if surface.exports.is_empty() {
            continue;
        }

        let Ok(out) = Command::new("wasm-tools").arg("component").arg("wit").arg(f).output() else {
            eprintln!("SKIPPED: wasm-tools is not on PATH");
            return None;
        };
        if !out.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        for (id, body) in interfaces_in(&text) {
            if !surface.exports.contains(&id) {
                continue;
            }
            match all.get(&id) {
                // Two artifacts rendering one INTERFACE differently means one of
                // them is stale — built against a shape that no longer exists.
                // That is this test's own subject, caught from the other side.
                Some(seen) if *seen != body => disagreements.push(format!(
                    "  {id} differs between artifacts — one of them is stale ({})",
                    f.file_name().unwrap_or_default().to_string_lossy()
                )),
                Some(_) => {}
                None => {
                    all.insert(id, body);
                }
            }
        }
    }
    assert!(
        disagreements.is_empty(),
        "one interface is two shapes across the built tree:\n{}\n\nRun `just build force=1`.",
        disagreements.join("\n")
    );
    Some(all)
}

fn render(all: &BTreeMap<String, String>) -> String {
    let mut s = String::from(
        "# Every WIT package this repository defines\n\
         \n\
         Generated — `just wit-surfaces`. Do not edit.\n\
         \n\
         Rendered by `wasm-tools component wit` out of the BUILT components, so this\n\
         is the shape that actually shipped rather than the shape the source suggests.\n\
         Doc comments are not part of it, so editing one does not churn this file.\n\
         \n\
         **The diff is the review.** A change here is a change to a contract. If a\n\
         package's shape moved and its version did not, `witsurface.rs` fails — and\n\
         it is right to, because an artifact built against the old shape will fail to\n\
         plug with a message that names neither the interface nor the reason.\n\
         \n\
         Adding a *function* to an interface is compatible; adding a case to a\n\
         *variant* or a field to a *record* is not. Both measured, not assumed.\n\n",
    );
    s.push_str(&format!("{} interfaces.\n\n", all.len()));
    for (id, body) in all {
        s.push_str(&format!("## `{id}`\n\n```wit\n"));
        s.push_str(body);
        s.push_str("\n```\n\n");
    }
    s
}

fn snapshot_path(root: &Path) -> PathBuf {
    root.join("wit/SURFACES.md")
}

/// The committed snapshot still describes what is built.
#[test]
fn the_committed_surfaces_are_not_stale() {
    let root = repo_root();
    let Some(all) = surfaces(&root) else { return };
    let path = snapshot_path(&root);

    if std::env::var("WIT_SURFACES").as_deref() == Ok("write") {
        std::fs::write(&path, render(&all)).expect("writing the snapshot");
        eprintln!("wrote {} interfaces to {}", all.len(), path.display());
        return;
    }

    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    let fresh = render(&all);
    assert_eq!(
        committed, fresh,
        "wit/SURFACES.md no longer describes what is built — run `just wit-surfaces`.\n\
         If a package's SHAPE changed, its version has to change with it."
    );
}

/// The rule the file exists for: a package's shape may not change under a version
/// that already had one.
///
/// Compared against the snapshot at `HEAD`, so bumping the version passes — the new
/// version simply is not in the old file — and changing a shape in place does not.
/// Skipped outside a git checkout, and on the first commit that adds the file.
/// `wit/SURFACES.md` as the base branch has it.
///
/// `origin/main` first, because that is what a pull request is measured against.
/// A local `main` is a fallback for a checkout with no remote, and it is a weaker
/// oracle — a stale local main compares against something old, which is wrong in
/// the safe direction: it reports a change that was already reviewed rather than
/// missing one that was not.
fn base_surfaces(root: &std::path::Path) -> Option<String> {
    for reference in ["origin/main:wit/SURFACES.md", "main:wit/SURFACES.md"] {
        let Ok(out) = Command::new("git").args(["show", reference]).current_dir(root).output()
        else {
            eprintln!("SKIPPED: git is not available");
            return None;
        };
        if out.status.success() {
            return Some(String::from_utf8_lossy(&out.stdout).into_owned());
        }
    }
    // A shallow clone has no `origin/main` — `actions/checkout` fetches one ref by
    // default. Loud, because a guard that quietly does not run is worse than one
    // that fails: the CI step that fetches main is in ci.yml next to this note.
    eprintln!(
        "SKIPPED: neither origin/main nor main has wit/SURFACES.md — \
         a shallow clone needs `git fetch --depth=1 origin main` first"
    );
    None
}

#[test]
fn a_shape_may_not_change_without_its_version() {
    let root = repo_root();
    let Some(all) = surfaces(&root) else { return };

    // Against the BASE BRANCH, not HEAD.
    //
    // `HEAD:wit/SURFACES.md` on a branch is a file you wrote yourself, and the
    // other test in this file — `the_committed_surfaces_are_not_stale` — tells you
    // to write it. So the workflow that satisfies one silenced the other:
    //
    //   1. add a case to a variant, leave the version alone   -> this test FAILS
    //   2. `just wit-surfaces`, commit (what the other demands)
    //   3. this test PASSES, and a breaking change ships
    //
    // Measured, not reasoned about: done to `qr:encode` on a scratch branch, and
    // it passed at step 3. Every version bump made while the oracle was HEAD is
    // unverified for the same reason.
    //
    // The base branch is the shape that SHIPPED. A branch cannot rewrite it by
    // regenerating a file, which is the whole property an oracle needs.
    let Some(base) = base_surfaces(&root) else { return };
    let out = base;
    let before = packages_in_markdown(&out);
    if before.is_empty() {
        eprintln!("SKIPPED: nothing to compare against");
        return;
    }

    let mut changed: Vec<String> = Vec::new();
    for (id, body) in &all {
        if let Some(was) = before.get(id) {
            if was != body {
                changed.push(format!("  {id}"));
            }
        }
    }
    assert!(
        changed.is_empty(),
        "these interfaces changed shape WITHOUT changing version:\n{}\n\n\
         An artifact built against the old shape will fail to plug, and the failure \
         names neither the interface nor the reason. Bump the version — see the note \
         at the top of components/graph-fitness/wit/fitness.wit for the one time this \
         was got wrong.",
        changed.join("\n")
    );
}

/// The fenced blocks of a rendered snapshot, back into a map.
fn packages_in_markdown(md: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut lines = md.lines();
    let mut current: Option<String> = None;
    while let Some(l) = lines.next() {
        if let Some(id) = l.strip_prefix("## `").and_then(|r| r.strip_suffix('`')) {
            current = Some(id.to_string());
            continue;
        }
        if l != "```wit" {
            continue;
        }
        let mut body = Vec::new();
        for b in lines.by_ref() {
            if b == "```" {
                break;
            }
            body.push(b.to_string());
        }
        if let Some(id) = current.take() {
            out.insert(id, body.join("\n"));
        }
    }
    out
}
