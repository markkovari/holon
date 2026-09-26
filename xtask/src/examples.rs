//! `cargo xtask stage-examples` — was `just examples-stage`.
//!
//! 58 of the jco examples' `.wasm` inputs were TRACKED, and all 50 with a
//! same-named component had drifted from it — not by a metadata stamp, by
//! thousands of bytes. So ~40 examples were exercising frozen components that no
//! longer existed anywhere else, and a green example said nothing about the
//! component it claimed to demonstrate. They are build outputs now (`**/*.wasm`
//! in .gitignore, and reconciler/tests/derived.rs refuses a tracked one), and
//! this is what writes them.
//!
//! Derived from each example's own `package.json` rather than listed here: a
//! hand-kept list of 58 is wrong the first time somebody adds an example, and
//! wrong silently.

use anyhow::{Context, Result};
use colored::Colorize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::build::build_components;
use crate::compose::{comp_plug, derive};
use crate::util::RELEASE;

/// Three examples ask for a bare name and need the COMPOSED artifact, because a
/// bare component leaves non-WASI imports for jco to emit as bare specifiers,
/// which Node rejects outright as a URL scheme (`protocol 'audit:'`). auth-guard
/// imports ratelimit:guard + audit:log/recorder, and both it and audit-log
/// import audit:log/types — a TYPES-ONLY interface nothing exports, so
/// composition cannot satisfy it either; that one is stubbed at transpile time
/// by the shims the package.json files point at.
fn wants_composed(stem: &str) -> bool {
    stem.ends_with(".composed") || matches!(stem, "audit_log" | "auth_guard" | "webhook_ingest")
}

/// An example that demonstrates a component in-process needs a DETERMINISTIC,
/// offline composition. Where the interface has several exporters, say which —
/// otherwise the pick is alphabetical and moves whenever somebody adds one.
/// `llm:inference/inference` has four, and the pick once moved to
/// `anthropic-provider`: jco-ai went from self-contained to needing a network
/// key and failed on an unresolvable `comp:secrets/reader`.
fn preferred_plug(base: &str) -> Option<&'static str> {
    match base {
        "ai_inference" => Some("llm_inference"),
        _ => None,
    }
}

/// Components whose crate name is not the name the example uses.
fn bare_alias(stem: &str) -> String {
    match stem {
        "eventbus" => "event_bus".to_string(),
        "lock" => "lock_mutex".to_string(),
        "timer" => "scheduler_timer".to_string(),
        other => other.replace('-', "_"),
    }
}

/// Every `<x>.wasm` a `jco transpile <x>.wasm` in this package.json's scripts reads.
fn jco_inputs(package_json: &Path) -> Result<BTreeSet<String>> {
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(package_json)?)
        .with_context(|| format!("parsing {}", package_json.display()))?;
    let mut out = BTreeSet::new();
    let Some(scripts) = v.get("scripts").and_then(|s| s.as_object()) else { return Ok(out) };
    for script in scripts.values().filter_map(|s| s.as_str()) {
        let words: Vec<&str> = script.split_whitespace().collect();
        for w in words.windows(3) {
            if w[0] == "jco" && w[1] == "transpile" && w[2].ends_with(".wasm") {
                out.insert(w[2].to_string());
            }
        }
    }
    Ok(out)
}

pub(crate) fn stage_examples() -> Result<()> {
    build_components(false)?;
    let plug = comp_plug()?;

    let mut examples: Vec<PathBuf> = fs::read_dir("examples")?
        .flatten()
        .map(|e| e.path().join("package.json"))
        .filter(|p| p.is_file())
        .collect();
    examples.sort();

    // jco-vet-clinic wants auth_guard composed too; compose each one once.
    let mut composed: std::collections::BTreeMap<String, String> = Default::default();
    let mut staged = 0;
    for pj in &examples {
        let dir = pj.parent().unwrap();
        for want in jco_inputs(pj)? {
            let stem = want.strip_suffix(".wasm").unwrap();
            let src = if wants_composed(stem) {
                // A component composed against its own imports, so the component
                // name is the artifact name with the suffix off and underscores
                // hyphenated.
                let base = stem.strip_suffix(".composed").unwrap_or(stem);
                if let Some(p) = composed.get(base) {
                    p.clone()
                } else {
                    let out = format!("components/target/{base}.composed.wasm");
                    let root = base.replace('_', "-");
                    if let Some(pin) = preferred_plug(base) {
                        // `--dir` wins over the release dir, so a directory holding
                        // one artifact pins which exporter comp-plug picks.
                        let pin_dir = PathBuf::from(format!("components/target/stage-pin-{base}"));
                        let _ = fs::remove_dir_all(&pin_dir);
                        fs::create_dir_all(&pin_dir)?;
                        fs::copy(
                            format!("{RELEASE}/{pin}.wasm"),
                            pin_dir.join(format!("{pin}.wasm")),
                        )?;
                        let output =
                            Command::new(&plug).arg("--dir").arg(&pin_dir).arg(&root).output()?;
                        let _ = fs::remove_dir_all(&pin_dir);
                        if !output.status.success() {
                            anyhow::bail!(
                                "comp-plug {root} failed: {}",
                                String::from_utf8_lossy(&output.stderr)
                            );
                        }
                        fs::copy(String::from_utf8_lossy(&output.stdout).trim(), &out)?;
                    } else {
                        derive(&plug, &root, &out)?;
                    }
                    composed.insert(base.to_string(), out.clone());
                    out
                }
            } else {
                format!("{RELEASE}/{}.wasm", bare_alias(stem))
            };
            fs::copy(&src, dir.join(&want))
                .with_context(|| format!("staging {src} -> {}/{want}", dir.display()))?;
            staged += 1;
        }
    }
    println!(
        "{}",
        format!("✔ Staged {staged} example input(s) from the build — none of them tracked")
            .green()
            .bold()
    );
    Ok(())
}
