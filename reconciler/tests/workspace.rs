//! Two things about the workspace that nothing else would notice breaking (docs/adr/0115).
//!
//! **Every package in it is a default member.** `cd reconciler && cargo test`, `cargo nextest run`,
//! `cargo clippy --all-targets` and every `--bin comp-x` that CI, the release workflow and the Docker
//! build spell act on the default members. A package that is a member and not a default one still
//! compiles — the root depends on it, or a sibling does — and then its tests are no longer run, its
//! binaries no longer built by name and its code no longer linted, with nothing red. It happened
//! while `comp-fleet` was being split out: four tests were missing from `cargo nextest list`, which is
//! how it was seen.
//!
//! **The crates whose types cross a package boundary are in the graph once.** A second copy of
//! `async-nats` is a second `Client` that is not the first one; a second `wasmparser` is a second
//! reader of the same component. Each compiled for as long as it stood, and each was a type error
//! waiting for the first call that passed one across — `comp-vcs` linked `holon-vcs` on 0.50 while the
//! rest of the repository, the host's wRPC transport included, was on 0.49.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn ids(meta: &Value, key: &str) -> BTreeSet<String> {
    meta[key]
        .as_array()
        .unwrap_or_else(|| panic!("cargo metadata has no `{key}`"))
        .iter()
        .map(|v| v.as_str().expect("a package id is a string").to_string())
        .collect()
}

#[test]
fn every_package_in_this_workspace_is_a_default_member() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(["metadata", "--no-deps", "--format-version", "1", "--manifest-path"])
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .output()
        .expect("run cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: Value = serde_json::from_slice(&out.stdout).expect("cargo metadata prints JSON");

    let missing: Vec<String> = ids(&meta, "workspace_members")
        .difference(&ids(&meta, "workspace_default_members"))
        .cloned()
        .collect();
    assert!(
        missing.is_empty(),
        "workspace members that are not in `default-members` in reconciler/Cargo.toml, so \
         `cargo test`, `cargo clippy` and `--bin` skip them: {missing:#?}"
    );
}

/// Crates whose types are handed between packages, with the reason, so a failure says what a second
/// copy would cost. Not every duplicate in a lockfile is a problem — `syn` 1 and 2 are two proc-macro
/// stacks that never meet — only the ones listed here are.
const ONE_COPY: &[(&str, &str)] = &[
    ("async-nats", "every daemon, holon-vcs and holon-park share one NATS client type"),
    ("wasmparser", "comp-fleet reads a component with the same reader wac-graph does"),
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// `name -> versions` for every `[[package]]` of a Cargo.lock. The format is one `name = "…"` line and
/// then one `version = "…"` line per package; a TOML parser would be a dependency for those two lines.
fn locked(lock: &Path) -> BTreeMap<String, BTreeSet<String>> {
    let text =
        std::fs::read_to_string(lock).unwrap_or_else(|e| panic!("read {}: {e}", lock.display()));
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let (mut in_package, mut name) = (false, None::<String>);
    for line in text.lines() {
        if line.starts_with('[') {
            (in_package, name) = (line == "[[package]]", None);
        } else if !in_package {
            continue;
        } else if let Some(n) = line.strip_prefix("name = \"") {
            name = n.strip_suffix('"').map(str::to_string);
        } else if let Some(v) = line.strip_prefix("version = \"") {
            if let (Some(n), Some(v)) = (name.take(), v.strip_suffix('"')) {
                found.entry(n).or_default().insert(v.to_string());
            }
        }
    }
    assert!(!found.is_empty(), "no packages read from {}", lock.display());
    found
}

/// The semver-compatible series a version belongs to: `0.49.1` is `0.49`, `1.4.2` is `1`.
fn series(version: &str) -> String {
    let mut parts = version.split('.');
    match parts.next() {
        Some("0") => format!("0.{}", parts.next().unwrap_or_default()),
        Some(major) => major.to_string(),
        None => String::new(),
    }
}

#[test]
fn the_reconciler_links_one_copy_of_each_crate_whose_types_cross_packages() {
    // `reconciler/Cargo.lock` is not committed (the root `.gitignore`), but `cargo test` has just
    // resolved it, which is the graph the binaries are built from.
    let lock = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.lock");
    let all = locked(&lock);
    let doubled: Vec<String> = ONE_COPY
        .iter()
        .filter_map(|(name, why)| {
            let versions = all.get(*name)?;
            (versions.len() > 1).then(|| format!("{name} {versions:?} — {why}"))
        })
        .collect();
    assert!(
        doubled.is_empty(),
        "more than one copy of a crate whose types cross packages in the reconciler's lock; give \
         the packages that name it one version requirement (`cargo tree -i <crate>` says who):\n{}",
        doubled.join("\n")
    );
}

#[test]
fn every_lockfile_that_runs_nats_runs_the_same_async_nats() {
    // The host cannot move alone — its wRPC transport is built on one client version — and the
    // lattice, the reconciler and the engine crates follow it, so a bump has to land in all four.
    let series_of: BTreeMap<&str, BTreeSet<String>> = ["reconciler", "host", "lattice", "crates"]
        .into_iter()
        .map(|dir| {
            let all = locked(&root().join(dir).join("Cargo.lock"));
            let versions = all
                .get("async-nats")
                .unwrap_or_else(|| panic!("{dir}/Cargo.lock has no async-nats"));
            (dir, versions.iter().map(|v| series(v)).collect())
        })
        .collect();
    let distinct: BTreeSet<&BTreeSet<String>> = series_of.values().collect();
    assert!(
        distinct.len() == 1,
        "the repository runs more than one async-nats series: {series_of:#?}"
    );
}
