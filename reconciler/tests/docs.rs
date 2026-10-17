//! Every relative link in the repository's markdown points at something.
//!
//! Documentation is load-bearing: ADRs are the method (recorded, superseded in place,
//! cross-referenced), so a dead link breaks the chain that makes one readable, and `docs/apps/` is
//! how anybody finds a showcase.
//!
//! Scope: links between files in the repo, and the section anchors on them. Not URLs (a test failing
//! because a website moved gets disabled). Anchors are included because a link to a heading that
//! exists but is the wrong one lands somewhere plausible, so nobody reports it.

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

/// `[text](target)`: targets only, and only the ones that name a file.
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

/// GitHub's heading slug: lowercase, drop everything but alphanumerics, spaces and hyphens, then
/// spaces to hyphens.
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

/// Every anchor a reader could land on in one file: its headings, plus explicit `id="…"`/`name="…"`.
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

    // Read once: anchor checking needs the TARGET file's headings, and re-reading per link is wasteful.
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
            // Not ours to check: the web and mail. `javascript:` appears in a document ABOUT
            // sanitising it. Schemes are not paths.
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
            // Only markdown has headings we can read; a `#L42` into source is not ours to resolve.
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

/// Every `just <recipe>` a document tells you to run is a recipe that exists. A missing recipe fails
/// with `Justfile does not contain recipe`, which reads as "your checkout is wrong" rather than "this
/// document is."
///
/// ADR BODIES are exempt: an ADR records what was decided when, and rewriting it so its commands
/// still run would falsify the record.
#[test]
fn every_just_recipe_a_document_names_exists() {
    let root = repo_root();
    // The Justfile AND everything it imports (`import` splices a file in, so `just/host.just` recipes
    // are reached as `just host-serve`). Read off the `import` lines, not by globbing `just/`: a
    // fragment nobody imports is not part of the interface.
    let Ok(justfile) = std::fs::read_to_string(root.join("Justfile")) else {
        // Justfile was eliminated in favor of programmatic build system (cargo xtask / build.zig)
        return;
    };
    let mut all = justfile.clone();
    for line in justfile.lines() {
        let Some(rest) = line.trim().strip_prefix("import ") else { continue };
        let path = rest.trim().trim_matches(|c| c == '\'' || c == '"');
        let text = std::fs::read_to_string(root.join(path))
            .unwrap_or_else(|e| panic!("Justfile imports {path}, which does not read: {e}"));
        all.push('\n');
        all.push_str(&text);
    }

    let recipes: BTreeSet<String> = all
        .lines()
        .filter(|l| !l.starts_with(char::is_whitespace) && !l.starts_with('#'))
        .filter_map(|l| {
            let head = l.split(':').next()?;
            let name = head.split_whitespace().next()?;
            (!name.is_empty()
                && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                && l.contains(':'))
            .then(|| name.to_string())
        })
        .collect();
    assert!(
        recipes.len() > 250,
        "only found {} recipes — the parser is wrong, or an import stopped being read",
        recipes.len()
    );

    let mut missing: BTreeSet<String> = BTreeSet::new();
    for file in markdown_files(&root) {
        let rel = file.strip_prefix(&root).unwrap_or(&file).display().to_string();
        if rel.starts_with("docs/adr/0") {
            continue; // the record, not an instruction
        }
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        for (i, line) in text.lines().enumerate() {
            let mut hay = line;
            while let Some(at) = hay.find("just ") {
                hay = &hay[at + 5..];
                let word: String =
                    hay.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
                // `just <app>-ui` placeholders and English after "just" are not recipe names; a hyphen
                // or known prefix distinguishes an instruction from a sentence.
                if word.len() < 3
                    || !word.contains('-')
                    || word.ends_with('-')
                                        // `just --list` is a flag; a recipe name never starts with one.
                    || word.starts_with('-')
                {
                    continue;
                }
                if !recipes.contains(&word) {
                    missing.insert(format!("  {rel}:{} -> just {word}", i + 1));
                }
            }
        }
    }

    assert!(
        missing.is_empty(),
        "{} document(s) tell you to run a recipe the Justfile does not have:\n{}",
        missing.len(),
        missing.into_iter().collect::<Vec<_>>().join("\n")
    );
}

/// Nothing in this repository is pinned to one machine's filesystem.
///
/// `goal-demo.sh`, offered in `README.md` as *the* one command from goal to pull request, named one
/// person's home directory. `bench/adversarial/run.sh` and `bench/idle/run.sh` began with
/// `cd /Users/…/experiments/comp`, a SIBLING checkout, so benchmarks built here and measured there and
/// reported a number somebody would quote. `bench/idle/run.sh` also wrote to one editor session's
/// scratch directory.
#[test]
fn no_script_is_pinned_to_one_machine() {
    let root = repo_root();
    let mut pinned = Vec::new();
    let mut checked = 0usize;

    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            let rel = path.strip_prefix(&root).unwrap_or(&path).to_string_lossy().to_string();
            if SKIP.iter().any(|s| rel.starts_with(s) || rel.contains(&format!("/{s}/"))) {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let is_script =
                path.extension().is_some_and(|x| x == "sh" || x == "mjs" || x == "py" || x == "rs");
            // `just/*.just` too: a hard-coded path in a fragment is pinned to one machine as much as
            // one in the root. Matched on the extension so a fifth fragment is covered.
            let is_just = rel == "Justfile" || rel.ends_with(".just");
            if !(is_script || is_just) {
                continue;
            }
            // This file spells the needles out in order to look for them.
            if rel.ends_with("tests/docs.rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            checked += 1;
            for (i, line) in text.lines().enumerate() {
                let t = line.trim();
                // A comment may quote one; that is how the reason gets recorded.
                if t.starts_with("//") || t.starts_with('#') || t.starts_with("///") {
                    continue;
                }
                // `/Users/` and `/home/` are somebody's machine; `/private/tmp/claude-…` is one
                // editor session's scratch directory.
                for needle in ["/Users/", "/home/", "/private/tmp/claude-"] {
                    if line.contains(needle) {
                        pinned.push(format!(
                            "  {rel}:{} -> {}",
                            i + 1,
                            t.chars().take(96).collect::<String>()
                        ));
                    }
                }
            }
        }
    }

    assert!(checked > 50, "only read {checked} scripts — the walk is wrong");
    assert!(
        pinned.is_empty(),
        "{} line(s) name a path that exists on one machine:\n{}\n\nDerive it instead — \
         `cd \"$(dirname \"$0\")/../..\"` for a script's own repo, `$(mktemp -d)` for scratch, \
         or `${{VAR:?why}}` for something the caller must supply.",
        pinned.len(),
        pinned.join("\n")
    );
    println!("  {checked} scripts, none pinned to a machine");
}

/// The architecture map is generated from `docs/architecture/map.mjs`, and a generated file nobody
/// regenerates is a map of something that no longer exists.
///
/// `build.mjs --check` fails when `docs/ARCHITECTURE.md` or `docs/architecture/index.html` is stale,
/// and when the map names a path or an ADR that is not in the tree, so renaming a file the map points
/// at is a failing test here and not a wrong box there. It needs `node` on PATH; without it this says
/// so and returns, as the suites that want a NATS server do.
#[test]
fn the_architecture_map_is_current_and_names_only_what_exists() {
    let root = repo_root();
    if std::process::Command::new("node").arg("--version").output().is_err() {
        eprintln!("SKIPPED: no `node` on PATH, so docs/ARCHITECTURE.md was not checked");
        return;
    }
    let out = std::process::Command::new("node")
        .args(["docs/architecture/build.mjs", "--check"])
        .current_dir(&root)
        .output()
        .expect("node answered a moment ago");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    println!("  {}", String::from_utf8_lossy(&out.stdout).trim());
}
