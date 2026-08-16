//! Every relative link in the repository's markdown points at something.
//!
//! Written after moving 35 files out of the repository root, which rewrote 121
//! files' references and 53 links inside the moved documents. That went fine, but
//! nothing would have caught it if it had not: the one broken link found in the
//! process — ADR-0057 pointing at ADR-0032 under a name it lost long ago — had
//! been broken for as long as the rename, silently.
//!
//! Documentation here is load-bearing in an unusual way. The ADRs are the method:
//! a decision is recorded, superseded in place and cross-referenced, so a link
//! that goes nowhere breaks the chain that makes an ADR readable at all. And the
//! `docs/apps/` index is how anybody finds a showcase now.
//!
//! Scope is deliberately narrow: relative links between files in the repo. Not
//! anchors (`#section`), not URLs — a test that fails because a website moved is a
//! test that gets disabled.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use comp_reconciler::fleet::repo_root;

/// Directories with no documentation of ours in them.
const SKIP: &[&str] = &["target", ".git", "node_modules", "vendor", "wit/deps"];

fn markdown_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().to_string();
            if SKIP.iter().any(|s| rel.starts_with(s) || rel.contains(&format!("/{s}/"))) {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// `[text](target)` — targets only, and only the ones that name a file.
fn links(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == ']' && i + 1 < bytes.len() && bytes[i + 1] == '(' {
            let mut j = i + 2;
            let mut depth = 1;
            let mut target = String::new();
            while j < bytes.len() && depth > 0 {
                match bytes[j] {
                    '(' => {
                        depth += 1;
                        target.push('(');
                    }
                    ')' => {
                        depth -= 1;
                        if depth > 0 {
                            target.push(')');
                        }
                    }
                    c => target.push(c),
                }
                j += 1;
            }
            out.push(target);
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

#[test]
fn every_relative_markdown_link_resolves() {
    let root = repo_root();
    let mut broken: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for file in markdown_files(&root) {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let dir = file.parent().unwrap_or(&root);
        for target in links(&text) {
            // Not ours to check: the web, in-page anchors, and mail.
            // `javascript:` appears inside a document ABOUT sanitising it, in
            // link syntax on purpose. Schemes are not paths.
            if target.starts_with("http")
                || target.starts_with('#')
                || target.starts_with("mailto:")
                || target.starts_with("javascript:")
            {
                continue;
            }
            // An anchor on a file still names a file.
            let path_part = target.split('#').next().unwrap_or(&target).trim();
            if path_part.is_empty() {
                continue;
            }
            checked += 1;
            let resolved = dir.join(path_part);
            if !resolved.exists() {
                let from = file.strip_prefix(&root).unwrap_or(&file).display();
                broken.push(format!("  {from} -> {path_part}"));
            }
        }
    }

    let unique: BTreeSet<_> = broken.into_iter().collect();
    assert!(
        unique.is_empty(),
        "{} relative link(s) point at nothing ({} checked):\n{}",
        unique.len(),
        checked,
        unique.into_iter().collect::<Vec<_>>().join("\n")
    );
    println!("  {checked} relative links, all of them resolve");
}
