//! Two things about the root workspace that nothing else would notice breaking (docs/adr/0115).
//!
//! **Every package in it is a default member.** `cargo test`, `cargo nextest run`, `cargo clippy` and
//! every `--bin comp-x` act on the default members. A member that is not one still compiles when a
//! sibling depends on it, and then its tests no longer run and its code is no longer linted, with
//! nothing red. It happened while `comp-fleet` was being split out.
//!
//! **The crates whose types cross a package boundary are in the graph once.** A second `async-nats`
//! is a second `Client` that is not the first; a second `wasmparser` is a second reader of the same
//! component. Each compiles until the first call passes one across.

use std::collections::BTreeSet;
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
        .arg(root().join("Cargo.toml"))
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
        "workspace members that are not in `default-members` in the root Cargo.toml, so \
         `cargo test`, `cargo clippy` and `--bin` skip them: {missing:#?}"
    );
}

/// Crates whose types are handed between packages, the packages whose graph must hold one copy
/// (none: the whole workspace), and why. Not every duplicate is a problem: wasmtime in `comp-host`
/// brings its own `wasmparser`, which never meets the one `comp-fleet` reads components with.
const ONE_COPY: &[(&str, &[&str], &str)] = &[
    ("async-nats", &[], "every daemon, the host's wRPC transport, holon-vcs and holon-park share one NATS client type"),
    ("wasmparser", &["comp-fleet"], "comp-fleet reads a component with the same reader wac-graph does"),
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The versions of `name` in the normal-dependency graph of `packages` (none: the whole workspace).
fn versions_in_graph(name: &str, packages: &[&str]) -> BTreeSet<String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(root())
        .args(["tree", "-q", "-e", "normal", "--prefix", "none", "--format", "{p}"]);
    if packages.is_empty() {
        cmd.arg("--workspace");
    }
    for p in packages {
        cmd.args(["-p", p]);
    }
    let out = cmd.output().expect("run cargo tree");
    assert!(out.status.success(), "cargo tree failed:\n{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix(&format!("{name} v")))
        .map(|v| v.split_whitespace().next().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn one_copy_of_each_crate_whose_types_cross_packages() {
    let doubled: Vec<String> = ONE_COPY
        .iter()
        .filter_map(|(name, packages, why)| {
            let versions = versions_in_graph(name, packages);
            (versions.len() > 1).then(|| format!("{name} {versions:?} — {why}"))
        })
        .collect();
    assert!(
        doubled.is_empty(),
        "more than one copy of a crate whose types cross packages; give the packages that name it \
         one version requirement (`cargo tree -i <crate>` says who):\n{}",
        doubled.join("\n")
    );
}
