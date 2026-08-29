//! The graph is derived, and deriving it does not write into the repository.
//!
//! The e2e that motivated this file: a new interface was added to `slug`, the
//! components were rebuilt, and the projection re-run. The store moved —
//! 120 interfaces to 121, 224 export edges to 225, `slug`'s digest changed, a
//! `slug:generate/validator@0.1.0` row appeared with zero consumers. `git status`
//! named two files, and both were the component's own sources.
//!
//! That is the property. It holds today by convention: nobody has wired a
//! generated file into the build, and `comp-capgraph` writes to stdout because the
//! `Justfile` owns the redirect. Convention is not a guard, and the failure it
//! invites is quiet — a tool that starts writing its own output leaves a repository
//! where every rebuild produces a diff, and the diff is noise that reviewers learn
//! to skip.
//!
//! Two things are checked, and the second is the one with teeth:
//!
//!   1. Every output format leaves the working tree exactly as it found it.
//!   2. The one graph snapshot that IS committed cannot drift from the components
//!      it claims to describe.
//!
//! The second is what makes deleting `docs/CAPABILITY-GRAPH.md` attractive rather
//! than merely tidy: while the file exists, every component change must carry a
//! regeneration of it, and that is a file change this repository would rather not
//! have. A guard that makes the cost visible is the honest way to argue for the
//! deletion.

use std::process::Command;

/// The formats `comp-capgraph` emits. `md` is included deliberately: it is the one
/// with a committed destination, so it is the one most likely to grow a write.
const FORMATS: &[&str] = &["json", "surql", "mermaid", "md"];

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// `git status --porcelain`, or `None` when git cannot answer — a checkout with no
/// git is a skip, not a pass.
fn tree_state(root: &std::path::Path) -> Option<String> {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root)
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn capgraph(format: &str) -> Option<Vec<u8>> {
    let out = Command::new(env!("CARGO_BIN_EXE_comp-capgraph"))
        .args(["--format", format])
        .current_dir(root())
        .output()
        .expect("comp-capgraph did not run");
    if !out.status.success() {
        eprintln!("SKIPPED: comp-capgraph refused --format {format} — run `just build`");
        return None;
    }
    Some(out.stdout)
}

/// Deriving the graph writes nothing. Every format, one comparison of the whole
/// working tree before and after.
///
/// Whole-tree rather than a list of known outputs on purpose: a list only catches
/// the files somebody thought of, and the failure worth catching is a NEW file
/// appearing.
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
             The tool emits to stdout; the Justfile owns any redirect. If a format \
             now needs a destination, give it to the recipe, not to the binary."
        );
    }
}

/// The committed snapshot says what the built components say.
///
/// This is the whole cost of keeping the file: it can be wrong, silently, from the
/// moment a component changes until somebody remembers `just capgraph`. The store
/// has no such failure mode — a projection is rewritten whole and stamped, so it is
/// either current or absent.
#[test]
fn the_committed_capability_graph_is_not_stale() {
    let root = root();
    let committed = root.join("docs/CAPABILITY-GRAPH.md");
    if !committed.exists() {
        // The intended end state: the graph lives in the store and nothing renders
        // it into the tree. Nothing to be stale.
        return;
    }
    let Some(fresh) = capgraph("md") else { return };
    let on_disk = std::fs::read(&committed).expect("docs/CAPABILITY-GRAPH.md is unreadable");
    assert!(
        fresh == on_disk,
        "docs/CAPABILITY-GRAPH.md disagrees with the built components — run `just capgraph`.\n\
         It is derived from the artifacts, so a component changed and the render did not."
    );
}

/// No build output is tracked.
///
/// 58 `.wasm` files were, and all 50 that had a same-named component had drifted
/// from it — `crdt.wasm` by 4 100 bytes, `cron.wasm` by 3 315. So around forty jco
/// examples were transpiling frozen copies of components that no longer existed,
/// and a green example proved nothing about the component it demonstrated.
///
/// `.gitignore` has carried `**/*.wasm` throughout. Git keeps tracking what it
/// already tracks, so the rule never reached the files that predated it, and
/// nothing said so — the failure is not a broken build, it is a passing test about
/// the wrong bytes. `just examples-stage` produces them from the build instead.
///
/// The extension list is deliberately short. This guards the case that actually
/// happened, and a guard that tries to name every possible build output is one
/// nobody can keep true.
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

    let tracked: Vec<&str> = listed
        .lines()
        .filter(|f| BUILT.iter().any(|e| f.ends_with(e)))
        .collect();

    assert!(
        tracked.is_empty(),
        "{} build artifact(s) are tracked, and a committed copy of something derived \
         goes stale without saying so:\n  {}\n\
         Stage them instead — `just examples-stage` writes every jco input from the build.",
        tracked.len(),
        tracked.join("\n  ")
    );
}
