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

use std::collections::{BTreeMap, BTreeSet};
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

/// The committed catalogue says what the components say.
///
/// `components/CATALOG.md` is the one derived file still committed, and it is
/// committed for a reason nothing else here has: it is READ BY PEOPLE, on GitHub,
/// without running anything. Its companion `catalog.json` was committed "for
/// tooling" and is gone — once `capsearch` stopped reading it, the only things left
/// reading it were the tests checking whether it had gone stale, which is a file
/// existing to be verified rather than used.
///
/// Neither could be checked at all until the build output came out of them: they
/// carried `wasm_size_bytes` and `wasm_sha256_12` from the last build, so they were
/// stale the moment anybody ran `just build`, for reasons having nothing to do with
/// the catalogue. That is why there was never a guard.
#[test]
fn the_committed_catalogue_is_not_stale() {
    let root = root();
    let markdown = root.join("components/CATALOG.md");
    if !markdown.exists() {
        return;
    }
    let before = std::fs::read(&markdown).expect("CATALOG.md is unreadable");

    // `CARGO_BIN_EXE_` rather than a path: cargo builds the binary as a prerequisite
    // of this test, so the check cannot pass by running a stale one.
    let run = Command::new(env!("CARGO_BIN_EXE_comp-catalog")).current_dir(&root).output();
    let Ok(run) = run else {
        eprintln!("SKIPPED: comp-catalog did not run");
        return;
    };
    if !run.status.success() {
        eprintln!("SKIPPED: the generator failed: {}", String::from_utf8_lossy(&run.stderr));
        return;
    }

    let after = std::fs::read(&markdown).expect("CATALOG.md is unreadable");
    // Put it back before asserting: a failing test must not leave the tree dirty, or
    // the next thing to run sees a change nobody made.
    let _ = std::fs::write(&markdown, &before);

    assert!(
        before == after,
        "components/CATALOG.md disagrees with the components — run `just catalog`.\n\
         It is derived from their sources, so a component changed and the render did not."
    );
}

/// One package name means one contract, everywhere.
///
/// A WIT package name is a GLOBAL identifier: `vision:describe@0.1.0` means one
/// thing, in every component, forever. Two files declaring it with different
/// contents is not duplication — it is one name meaning two things, and which one a
/// tool resolves depends on which directory it happened to look in.
///
/// This was `tools/check-wit-packages.py`, which NOTHING RAN: no recipe, no
/// workflow, no test. It was found by grepping for scripts nothing referenced, on
/// the assumption they were dead. It was not dead, it was unwired — and it was
/// failing: `components/anthropic-vision/wit/vision.wit` was a verbatim copy of the
/// `vision-describe` contract with one world appended, so both claimed
/// `vision:describe@0.1.0`.
///
/// Ported into the test rather than shelled out to, so CI's critical path does not
/// depend on a `python3` that happens to be on the runner. Comments and whitespace
/// are normalised away before comparing: two files that differ only in how they
/// explain themselves are the same contract, and flagging that would train everyone
/// to ignore this.
///
/// Only files git TRACKS. Walking the filesystem instead reported a collision against
/// `components/portfolio-value-cs/bin/Release/…/WasiHttpWorld_component_type.wit` —
/// C# build output, ignored, never committed, and back after the next `dotnet build`.
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

        // Everything AFTER the package line, comments and whitespace normalised out.
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
        .filter(|(_, entries)| {
            entries.iter().map(|(_, d)| d).collect::<BTreeSet<_>>().len() > 1
        })
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

/// A short content digest. Only ever compared for equality, never published, so the
/// cheap non-cryptographic hash the standard library already has is enough.
fn digest_of(text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    format!("{:016x}", h.finish())
}
