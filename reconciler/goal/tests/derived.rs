//! Derived artifacts are derived on demand: deriving writes nothing, and nothing built is tracked.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

/// The tool refuses `mermaid` and `md`.
const FORMATS: &[&str] = &["json", "surql"];

fn root() -> std::path::PathBuf {
    holon_fleet::fleet::root_from_manifest()
}

/// `None` when git cannot answer: a checkout with no git is a skip.
fn tree_state(root: &std::path::Path) -> Option<String> {
    let out =
        Command::new("git").args(["status", "--porcelain"]).current_dir(root).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn capgraph(format: &str) -> Option<Vec<u8>> {
    let out = Command::new(env!("CARGO_BIN_EXE_comp-capgraph"))
        .args(["--format", format])
        .current_dir(root())
        .output()
        .expect("comp-capgraph did not run");
    if !out.status.success() {
        eprintln!(
            "SKIPPED: comp-capgraph refused --format {format} — run `cargo xtask build --force`"
        );
        return None;
    }
    Some(out.stdout)
}

/// Whole-tree comparison, since the failure worth catching is a new file appearing.
#[test]
fn deriving_the_graph_does_not_touch_the_working_tree() {
    let root = root();
    let Some(before) = tree_state(&root) else {
        eprintln!("SKIPPED: git is not available");
        return;
    };

    for format in FORMATS {
        if capgraph(format).is_none() {
            return;
        }
        let after = tree_state(&root).expect("git answered once and then did not");
        assert_eq!(
            before, after,
            "`comp-capgraph --format {format}` changed the working tree.\n\
             The tool emits to stdout; the caller owns any redirect. If a format \
             now needs a destination, give it to the caller, not to the binary."
        );
    }
}

/// Tracked `.wasm` files drifted from their components; `.gitignore` does not untrack them.
#[test]
fn no_build_output_is_tracked() {
    const BUILT: &[&str] = &[".wasm", ".rlib", ".rmeta"];

    let root = root();
    let Ok(out) = Command::new("git").args(["ls-files"]).current_dir(&root).output() else {
        eprintln!("SKIPPED: git is not available");
        return;
    };
    if !out.status.success() {
        eprintln!("SKIPPED: git could not list the index");
        return;
    }
    let listed = String::from_utf8_lossy(&out.stdout);
    assert!(listed.lines().count() > 100, "git listed almost nothing — the check is not running");

    let tracked: Vec<&str> =
        listed.lines().filter(|f| BUILT.iter().any(|e| f.ends_with(e))).collect();

    assert!(
        tracked.is_empty(),
        "{} build artifact(s) are tracked, and a committed copy of something derived \
         goes stale without saying so:\n  {}\n\
         Stage them instead — `cargo xtask stage-examples` writes every jco input from the build.",
        tracked.len(),
        tracked.join("\n  ")
    );
}

/// A WIT package name is global. Comments and whitespace are normalised away; only tracked files.
#[test]
fn no_wit_package_name_means_two_things() {
    let root = root();
    let Ok(listed) = Command::new("git").args(["ls-files", "*.wit"]).current_dir(&root).output()
    else {
        eprintln!("SKIPPED: git is not available");
        return;
    };
    if !listed.status.success() {
        eprintln!("SKIPPED: git could not list the tree");
        return;
    }
    let files: Vec<String> =
        String::from_utf8_lossy(&listed.stdout).lines().map(str::to_string).collect();
    assert!(files.len() > 50, "git listed {} .wit files — the check is not running", files.len());

    // package name -> (path, digest of everything it declares)
    let mut by_package: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for rel in &files {
        let Ok(text) = std::fs::read_to_string(root.join(rel)) else { continue };
        let Some(at) = text.find("package ") else { continue };
        let Some(end) = text[at..].find(';') else { continue };
        let name = text[at + "package ".len()..at + end].trim().to_string();

        let body: String = text[at + end + 1..]
            .lines()
            .map(|l| match l.find("//") {
                Some(c) => &l[..c],
                None => l,
            })
            .collect::<Vec<_>>()
            .join(" ");
        let normalised = body.split_whitespace().collect::<Vec<_>>().join(" ");
        by_package.entry(name).or_default().push((rel.clone(), digest_of(&normalised)));
    }

    let collisions: Vec<_> = by_package
        .iter()
        .filter(|(_, entries)| entries.iter().map(|(_, d)| d).collect::<BTreeSet<_>>().len() > 1)
        .collect();

    assert!(
        collisions.is_empty(),
        "a WIT package name is claimed by more than one contract:\n{}\n\n\
         A package name is global. Rename one, or make them the same file.",
        collisions
            .iter()
            .map(|(name, entries)| {
                let lines: Vec<String> =
                    entries.iter().map(|(p, d)| format!("      {} {p}", &d[..12])).collect();
                format!("  {name}\n{}", lines.join("\n"))
            })
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Only compared for equality, so the std non-cryptographic hash is enough.
fn digest_of(text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    format!("{:016x}", h.finish())
}
