//! The one thing about the workspace nothing else would notice breaking (docs/adr/0115): every
//! package in it is a default member.
//!
//! `cd reconciler && cargo test`, `cargo nextest run`, `cargo clippy --all-targets` and every
//! `--bin comp-x` that CI, the release workflow and the Docker build spell act on the default
//! members. A package that is a member and not a default one still compiles — the root depends on
//! it, or a sibling does — and then its tests are no longer run, its binaries no longer built by
//! name and its code no longer linted, with nothing red. It happened while `comp-fleet` was being
//! split out: four tests were missing from `cargo nextest list`, which is how it was seen.

use std::collections::BTreeSet;
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
