//! What a goal's app is made of, and how much of it the run had to write.
//!
//!     comp-reuse-ratio .comp/goals/triage-assist.toml [...]
//!
//! Two numbers per app, each from a different source so one cannot be talked up:
//!
//!   COMPOSITION   the components `comp-plug` actually wires in, derived from the
//!                 compiled artifact's imports — not from a list anybody
//!                 maintains by hand.
//!   CAPABILITIES  the interfaces the compiled component IMPORTS, against the
//!                 ones its world offers. A world can offer a capability a part
//!                 never calls; only the import proves it was reached for, which
//!                 is what the gates assert.
//!
//! The ratio is component-based: reused / (reused + written). It is a ratio of
//! what EXISTS to what was AUTHORED for this app, which is the question "did the
//! pool carry the weight".
//!
//! `REUSE_ROOT` points this at another checkout — a worktree of a landed pull
//! request, which is the only tree where the written side is the run's actual
//! work rather than the stubs the repository keeps.

use regex::Regex;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn measured_root(repo: &Path) -> PathBuf {
    std::env::var("REUSE_ROOT").map(PathBuf::from).unwrap_or_else(|_| repo.to_path_buf())
}

/// Where built components are looked for: the measured tree first, the
/// repository second — earlier directories win, the same catalogue `comp-plug`
/// uses in a gate.
fn target_dirs(root: &Path, repo: &Path) -> Vec<String> {
    let mut dirs = Vec::new();
    for r in [root, repo] {
        for d in ["wasm32-wasip2", "wasm32-wasip1"] {
            let path = r.join("components/target").join(d).join("debug");
            if path.is_dir() {
                dirs.push("--dir".into());
                dirs.push(path.display().to_string());
            }
        }
    }
    dirs
}

fn plugs(repo: &Path, root: &Path, crate_name: &str) -> Vec<String> {
    let re = Regex::new(r"plugs: (.*)").unwrap();
    let Ok(out) = Command::new(repo.join("reconciler/target/release/comp-plug"))
        .arg(crate_name)
        .arg("--wiring")
        .args(target_dirs(root, repo))
        .current_dir(root)
        .output()
    else {
        return Vec::new();
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    re.captures(&stdout)
        .map(|c| c[1].split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect())
        .unwrap_or_default()
}

/// The interfaces the UNCOMPOSED artifact imports — what the code actually calls.
fn artifact_imports(root: &Path, crate_name: &str) -> Vec<String> {
    let wasm = format!("{}.wasm", crate_name.replace('-', "_"));
    let re = Regex::new(r"import ([a-z0-9:-]+/[a-z0-9-]+)").unwrap();
    for d in ["wasm32-wasip2", "wasm32-wasip1"] {
        let path = root.join("components/target").join(d).join("debug").join(&wasm);
        if !path.exists() {
            continue;
        }
        let Ok(out) = Command::new("wasm-tools").args(["component", "wit"]).arg(&path).output() else {
            continue;
        };
        let wit = String::from_utf8_lossy(&out.stdout);
        return unique_sorted(re.captures_iter(&wit).map(|c| c[1].to_string()));
    }
    Vec::new()
}

fn world_imports(root: &Path, crate_name: &str) -> Vec<String> {
    let re = Regex::new(r"import ([a-z0-9:-]+/[a-z0-9-]+)@").unwrap();
    let Some(text) = find_wit(&root.join("components").join(crate_name).join("wit")) else {
        return Vec::new();
    };
    unique_sorted(re.captures_iter(&text).map(|c| c[1].to_string()))
}

fn unique_sorted(items: impl Iterator<Item = String>) -> Vec<String> {
    let mut v: Vec<String> = items.collect();
    v.sort();
    v.dedup();
    v
}

/// The first `.wit` file found walking `dir` — matches Python's `os.walk`,
/// which returned on the first hit.
fn find_wit(dir: &Path) -> Option<String> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "wit") {
                return std::fs::read_to_string(&path).ok();
            }
        }
    }
    None
}

struct Row {
    reused: usize,
    written: usize,
}

fn report(repo: &Path, root: &Path, goal_path: &str) -> Option<Row> {
    let text = std::fs::read_to_string(goal_path).ok()?;
    let goal: toml::Value = toml::from_str(&text).ok()?;
    let title = goal.get("title").and_then(|t| t.as_str()).unwrap_or(goal_path).to_string();

    let written_files: Vec<String> = goal
        .get("part")
        .and_then(|p| p.as_array())
        .into_iter()
        .flatten()
        .filter_map(|p| p.get("writable").and_then(|w| w.as_array()))
        .flatten()
        .filter_map(|w| w.as_str())
        .filter(|w| w.ends_with(".rs"))
        .map(String::from)
        .collect();

    let crate_name = written_files.iter().find_map(|w| {
        let parts: Vec<&str> = w.split('/').collect();
        (parts.len() > 2 && parts[0] == "components").then(|| parts[1].to_string())
    })?;

    // A stub tree would report a 99% ratio against sixteen lines of `not_implemented`,
    // which is true and meaningless. Say so rather than print it as a result.
    let stub_count = written_files
        .iter()
        .filter(|w| std::fs::read_to_string(root.join(w)).is_ok_and(|c| c.contains(r#"501, "not_implemented""#)))
        .count();
    if stub_count > 0 {
        println!("\n=== {title}");
        println!(
            "    SKIPPED — {stub_count} of {} written file(s) are still stubs in this tree.\n    Measure a landed branch instead: REUSE_ROOT=<worktree> comp-reuse-ratio {goal_path}",
            written_files.len()
        );
        return None;
    }

    let wired = plugs(repo, root, &crate_name);
    let reused = wired.len();
    let written = 1; // the goal writes exactly its own crate
    let offered = world_imports(root, &crate_name);
    let called = artifact_imports(root, &crate_name);
    let reached = offered.iter().filter(|i| called.contains(i)).count();

    let ratio = reused as f64 / (reused + written) as f64;
    println!("\n=== {title}");
    println!("    crate: {crate_name}");
    println!("  COMPOSITION  {} component(s) wired in: {}", wired.len(), wired.join(", "));
    println!("  COMPONENTS   reused {reused} component(s)  ·  written {written} component(s)");
    println!(
        "               reuse ratio {:.1}%  — {}x more existing components than authored",
        ratio * 100.0,
        reused / written
    );
    println!("  CAPABILITIES world offers {}, artifact imports {reached}:", offered.len());
    for i in &offered {
        let tag = if called.contains(i) { "called  " } else { "UNUSED  " };
        println!("      {tag} {i}");
    }
    Some(Row { reused, written })
}

fn main() {
    let repo = repo();
    let root = measured_root(&repo);
    let goal_paths: Vec<String> = std::env::args().skip(1).collect();
    let rows: Vec<Row> = goal_paths.iter().filter_map(|g| report(&repo, &root, g)).collect();
    if rows.len() > 1 {
        let (tr, tw) = rows.iter().fold((0usize, 0usize), |(a, b), r| (a + r.reused, b + r.written));
        println!(
            "\n=== all {} app(s): reused {tr} components, written {tw} components, ratio {:.1}%",
            rows.len(),
            100.0 * tr as f64 / (tr + tw) as f64
        );
    }
}
