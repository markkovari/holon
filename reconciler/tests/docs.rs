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
//! Scope is deliberately narrow: links between files in the repo, and the section
//! anchors on them. Not URLs — a test that fails because a website moved is a test
//! that gets disabled.
//!
//! Anchors were out of scope until `CONTEXT.md` was found linking the word
//! "interface" at `#capability`, a heading that exists and is the wrong one. That
//! is worse than a dead link: it lands somewhere plausible, so nobody reports it.
//! The URL argument for skipping them never applied — a heading in a file in this
//! repository is exactly as checkable as a path to it.

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

/// GitHub's heading slug: lowercase, drop everything that is not alphanumeric,
/// space or hyphen, then spaces to hyphens.
fn slug(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '_')
        .collect::<String>()
        .trim()
        .replace(' ', "-")
}

/// Every anchor a reader could land on in one file: its headings, plus any
/// explicit `id="…"`/`name="…"` a hand-written HTML anchor declares.
fn anchors(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if let Some(rest) = line.trim_start().strip_prefix('#') {
            let title = rest.trim_start_matches('#').trim();
            if !title.is_empty() {
                // Markdown link syntax inside a heading renders as its text.
                let plain = title.replace("](", " ").replace(['[', ']', '(', ')'], "");
                out.insert(slug(&plain));
                out.insert(slug(title));
            }
        }
        for attr in ["id=\"", "name=\""] {
            let mut hay = line;
            while let Some(at) = hay.find(attr) {
                hay = &hay[at + attr.len()..];
                if let Some(end) = hay.find('"') {
                    out.insert(hay[..end].to_lowercase());
                }
            }
        }
    }
    out
}

#[test]
fn every_relative_markdown_link_resolves() {
    let root = repo_root();
    let mut broken: Vec<String> = Vec::new();
    let mut checked = 0usize;
    let mut anchors_checked = 0usize;

    // Read once. Anchor checking needs the TARGET file's headings, and the docs
    // tree is crossed by enough links that re-reading per link is wasteful.
    let files = markdown_files(&root);
    let anchors_of: std::collections::BTreeMap<PathBuf, BTreeSet<String>> = files
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok().map(|t| (f.clone(), anchors(&t))))
        .collect();

    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else { continue };
        let dir = file.parent().unwrap_or(&root);
        let from = file.strip_prefix(&root).unwrap_or(file).display().to_string();
        for target in links(&text) {
            // Not ours to check: the web and mail. `javascript:` appears inside a
            // document ABOUT sanitising it, in link syntax on purpose. Schemes are
            // not paths.
            if target.starts_with("http")
                || target.starts_with("mailto:")
                || target.starts_with("javascript:")
            {
                continue;
            }
            let (path_part, anchor) = match target.split_once('#') {
                Some((p, a)) => (p.trim(), Some(a.trim())),
                None => (target.trim(), None),
            };

            // Where the anchor, if there is one, has to exist.
            let target_file = if path_part.is_empty() {
                file.clone()
            } else {
                checked += 1;
                let resolved = dir.join(path_part);
                if !resolved.exists() {
                    broken.push(format!("  {from} -> {path_part}"));
                    continue;
                }
                resolved
            };

            let Some(anchor) = anchor.filter(|a| !a.is_empty()) else { continue };
            // Only markdown has headings we can read. A `#L42` into source, or an
            // anchor on anything else, is not ours to resolve.
            let Some(have) = anchors_of.get(&target_file) else { continue };
            anchors_checked += 1;
            if !have.contains(&anchor.to_lowercase()) {
                broken.push(format!("  {from} -> {target} (no such section)"));
            }
        }
    }

    let unique: BTreeSet<_> = broken.into_iter().collect();
    assert!(
        unique.is_empty(),
        "{} link(s) point at nothing ({checked} paths, {anchors_checked} anchors checked):\n{}",
        unique.len(),
        unique.into_iter().collect::<Vec<_>>().join("\n")
    );
    println!("  {checked} relative links and {anchors_checked} anchors, all of them resolve");
}
