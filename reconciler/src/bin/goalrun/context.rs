//! What a branch or a part is SHOWN — its writable files, its read-only
//! context, and (for a part) what the pool already has.

use serde_json::{json, Value};

use crate::pool;
use crate::PartSpec;

/// The files a branch is shown: what it may write, then what it may only read.
///
/// One function because `run_parts` builds the same thing for a part, and two
/// builders drift — the rehearsal and `goalrun` disagreeing about `component =` is
/// what that looks like when it happens.
///
/// Writable files keep every comment: there the comments are the brief. Read-only
/// `.wit` context is trimmed by `lean_context`; a `.rs` held-out test is not, because
/// its doc comments are the specification.
///
/// Deduped, and `writable` wins. A path in both lists would otherwise be sent twice
/// — paid for on every attempt, and the second copy stripped differently from the
/// first, which is a worse bug than the cost.
pub(crate) fn branch_context(
    checkout: &std::path::Path,
    writable: &[String],
    readonly: &[String],
) -> Vec<Value> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for (path, strip) in
        writable.iter().map(|w| (w, false)).chain(readonly.iter().map(|r| (r, true)))
    {
        if !seen.insert(path.as_str()) {
            continue;
        }
        match std::fs::read_to_string(checkout.join(path)) {
            Ok(c) => out.push(json!({
                "path": path,
                "content": if strip { pool::lean_context(path, c) } else { c },
            })),
            // Loud, because a typo'd context path is otherwise silent: the branch is
            // simply not shown the file and writes blind again, which is the failure
            // the field exists to remove.
            Err(e) => {
                println!("context: `{path}` could not be read ({e}) — the branch will not see it")
            }
        }
    }
    out
}

/// What a PART is shown: its own files, plus what the pool already has.
///
/// A function rather than two lines inside the plan literal so a test can ask the
/// question that matters — does a part read the capability search — without a
/// fleet, a contract registry and a model behind it. The decomposed path shipped
/// without this for as long as it did because nothing could see the difference
/// between a part told about `auth-guard` and a part not told (ADR-0094).
pub(crate) fn part_context(
    checkout: &std::path::Path,
    writable: &[String],
    readonly: &[String],
    pool: Option<Value>,
) -> Vec<Value> {
    let mut out = branch_context(checkout, writable, readonly);
    // LAST, and appended rather than merged: it is the only entry here that is
    // prose about other components instead of a file the part may read.
    out.extend(pool);
    out
}

/// The first line of a multi-line failure, for a one-line report.
pub(crate) fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s).trim()
}

/// Which part a join failure is about.
///
/// A join failure is the one verdict in a decomposed run that no part owns: the
/// halves each passed, the whole did not, and the run ends there. Naming an owner is
/// the first half of making it addressable — without it the reader is handed "the
/// halves pass alone and not together" and a diff of three parts.
///
/// Attribution is by evidence only: a part owns the failure if the text names one of
/// its writable paths, or names the part itself. A failure that names nothing owned
/// belongs to the JOIN — the contract, or the composition check — and saying so is
/// more useful than picking the likeliest part.
pub(crate) fn join_failure_owners(failure: &str, parts: &[PartSpec]) -> Vec<String> {
    let owners: Vec<String> = parts
        .iter()
        .filter(|p| {
            p.writable.iter().any(|w| failure.contains(w.as_str())) || failure.contains(&p.name)
        })
        .map(|p| p.name.clone())
        .collect();
    owners
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path in both `writable` and `context` is sent once, and as the WRITABLE
    /// copy — the untrimmed one. Sent twice it is paid for on every attempt, and the
    /// two copies are stripped differently, which is worse than the cost.
    #[test]
    fn a_path_in_both_lists_is_shown_once_and_unstripped() {
        let dir = std::env::temp_dir().join("holon-branch-context-test");
        let wit = dir.join("a.wit");
        std::fs::create_dir_all(&dir).expect("tmpdir");
        std::fs::write(&wit, "// a comment\npackage a:b@0.1.0;\n").expect("write");

        let both = vec!["a.wit".to_string()];
        let out = branch_context(&dir, &both, &both);
        assert_eq!(out.len(), 1, "shown once: {out:?}");
        assert!(
            out[0]["content"].as_str().expect("content").contains("// a comment"),
            "the writable copy keeps its comments: {out:?}"
        );

        // Read-only only: `lean_context` strips a `.wit`'s comments.
        let readonly = branch_context(&dir, &[], &both);
        assert!(!readonly[0]["content"].as_str().expect("content").contains("// a comment"));

        // A `.rs` held-out test is never stripped, in either position: its doc
        // comments are the specification.
        std::fs::write(dir.join("t.rs"), "//! the spec\nfn x() {}\n").expect("write");
        let rs = branch_context(&dir, &[], &["t.rs".to_string()]);
        assert!(rs[0]["content"].as_str().expect("content").contains("//! the spec"));

        // A path that does not exist is skipped, not fatal, and not an empty entry.
        assert!(branch_context(&dir, &[], &["nope.rs".to_string()]).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A PART reads what the pool already has, and reads it last.
    ///
    /// The capability search is mandatory in both directions (ADR-0094) and the
    /// decomposed path skipped it entirely: it ran below the dispatch that returns
    /// into `decomposed`, so a two-part run asked nothing and every part wrote
    /// blind. Nothing failed — an absent question looks exactly like one with no
    /// answer, which is the shape of failure this repository keeps rediscovering.
    #[test]
    fn a_part_is_told_what_the_pool_already_has() {
        let dir = std::env::temp_dir().join("holon-part-context-test");
        std::fs::create_dir_all(&dir).expect("tmpdir");
        std::fs::write(dir.join("own.rs"), "fn stub() {}\n").expect("write");
        let own = vec!["own.rs".to_string()];

        let pool = json!({"path": "the pool", "content": "- `auth-guard` exports auth:identity"});
        let with = part_context(&dir, &own, &[], Some(pool.clone()));
        assert_eq!(with.len(), 2, "its own file and the pool: {with:?}");
        assert_eq!(with[1], pool, "the pool entry is last");
        assert!(with[0]["content"].as_str().expect("content").contains("fn stub"));

        // No hits is no entry — not an empty one. A part shown "the pool has:"
        // followed by nothing reads as a pool that is empty rather than as a
        // question that found no answer.
        let without = part_context(&dir, &own, &[], None);
        assert_eq!(without.len(), 1, "nothing appended: {without:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A join failure is attributed by EVIDENCE — a path the part owns, or its name —
    /// and a failure naming neither belongs to the join itself. Guessing the likeliest
    /// part would be worse than saying so: the reader would go and read the wrong diff.
    #[test]
    fn a_join_failure_names_the_part_that_owns_it_or_says_it_owns_none() {
        let part = |name: &str, w: &str| PartSpec {
            name: name.into(),
            text: "t".into(),
            writable: vec![w.into()],
            context: vec![],
            checks: vec![],
        };
        let parts = vec![
            part("backend", "components/x/src/api.rs"),
            part("frontend", "components/x/ui/app.tsx"),
        ];

        assert_eq!(
            join_failure_owners("assertion failed at components/x/src/api.rs:22", &parts),
            vec!["backend"],
            "a path it owns"
        );
        assert_eq!(
            join_failure_owners("the frontend never called /api/total", &parts),
            vec!["frontend"],
            "its own name"
        );
        assert_eq!(
            join_failure_owners(
                "components/x/src/api.rs disagrees with components/x/ui/app.tsx",
                &parts
            ),
            vec!["backend", "frontend"],
            "both, when both are named — a boundary disagreement is not one part's"
        );
        assert!(
            join_failure_owners("the halves pass alone and not together (score 400)", &parts)
                .is_empty(),
            "names no part, so the JOIN owns it"
        );
    }
}
