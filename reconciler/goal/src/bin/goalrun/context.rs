use serde_json::{json, Value};

use crate::pool;
use crate::PartSpec;

/// Shared with `run_parts`: two builders drift. Writable files keep every comment (the brief), and
/// `writable` wins a duplicate, which would otherwise be paid for on every attempt.
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
            // Loud: a typo'd context path otherwise leaves the branch writing blind.
            Err(e) => {
                println!("context: `{path}` could not be read ({e}) — the branch will not see it")
            }
        }
    }
    out
}

/// Testable without a fleet: parts once shipped never told about `auth-guard` (ADR-0094).
pub(crate) fn part_context(
    checkout: &std::path::Path,
    writable: &[String],
    readonly: &[String],
    pool: Option<Value>,
) -> Vec<Value> {
    let mut out = branch_context(checkout, writable, readonly);
    // Last, appended not merged: it is prose about other components, not a file.
    out.extend(pool);
    out
}

pub(crate) fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s).trim()
}

/// By evidence only: a failure naming no part's path or name belongs to the join itself.
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

    #[test]
    fn a_path_in_both_lists_is_shown_once_and_unstripped() {
        let dir = std::env::temp_dir().join("holon-branch-context-test"); // nosemgrep: rust.lang.security.temp-dir.temp-dir
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

        let readonly = branch_context(&dir, &[], &both);
        assert!(!readonly[0]["content"].as_str().expect("content").contains("// a comment"));

        // A held-out `.rs` test is never stripped: its doc comments are the spec.
        std::fs::write(dir.join("t.rs"), "//! the spec\nfn x() {}\n").expect("write");
        let rs = branch_context(&dir, &[], &["t.rs".to_string()]);
        assert!(rs[0]["content"].as_str().expect("content").contains("//! the spec"));

        assert!(branch_context(&dir, &[], &["nope.rs".to_string()]).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_part_is_told_what_the_pool_already_has() {
        let dir = std::env::temp_dir().join("holon-part-context-test"); // nosemgrep: rust.lang.security.temp-dir.temp-dir
        std::fs::create_dir_all(&dir).expect("tmpdir");
        std::fs::write(dir.join("own.rs"), "fn stub() {}\n").expect("write");
        let own = vec!["own.rs".to_string()];

        let pool = json!({"path": "the pool", "content": "- `auth-guard` exports auth:identity"});
        let with = part_context(&dir, &own, &[], Some(pool.clone()));
        assert_eq!(with.len(), 2, "its own file and the pool: {with:?}");
        assert_eq!(with[1], pool, "the pool entry is last");
        assert!(with[0]["content"].as_str().expect("content").contains("fn stub"));

        // No hits, no entry: a heading over nothing reads as an empty pool.
        let without = part_context(&dir, &own, &[], None);
        assert_eq!(without.len(), 1, "nothing appended: {without:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

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
