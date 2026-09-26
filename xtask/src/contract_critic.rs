//! `cargo xtask contract-critic` — was tools/contract-critic.py.
//!
//! The cheap, deterministic half of what a branch used to do by hand: read a
//! goal's contract, find a claim the components do not support, before a
//! generation was spent on it. Two shapes, no model call:
//!   - a context file that does not resolve;
//!   - a capability a part's world imports whose signature the contract never
//!     quotes, so the part has to guess it.
//!
//! Ported from Python because it's a dev-tooling script with no component tie
//! — this repo's own convention keeps those in Rust alongside xtask.

use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// package namespace -> the binding alias a contract quotes it under
/// (e.g. `money:amount` is quoted as `money::`, `idempotency:*` as `idem::`).
fn contract_alias(ns: &str) -> &str {
    match ns {
        "idempotency" => "idem",
        "ratelimit" => "rl",
        "quota" => "meter",
        "otp" => "totp",
        "session" => "sessions",
        "event" => "bus",
        "auth" => "authz",
        "lock" => "mutex",
        other => other,
    }
}

/// The namespaces a `.wit` file's world imports (`import wasi:http/...;` -> `wasi`).
fn wit_imports(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("import "))
        .filter_map(|rest| rest.split(':').next())
        .map(str::to_string)
        .collect()
}

fn contract_critic_one(goal_path: &Path) -> Result<bool> {
    let goal: toml::Value = toml::from_str(
        &fs::read_to_string(goal_path)
            .with_context(|| format!("reading {}", goal_path.display()))?,
    )?;
    let title = goal
        .get("title")
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| goal_path.display().to_string());

    let Some(parts) = goal.get("part").and_then(|p| p.as_array()) else {
        println!("{}: single-part goal, nothing to cross-check", goal_path.display());
        return Ok(true);
    };
    let contract_path = goal
        .get("contract")
        .and_then(|c| c.as_str())
        .with_context(|| format!("{}: no `contract` key", goal_path.display()))?;
    let contract = fs::read_to_string(contract_path)
        .with_context(|| format!("reading contract {contract_path}"))?;

    let mut problems: BTreeSet<String> = BTreeSet::new();
    for part in parts {
        let name = part.get("name").and_then(|n| n.as_str()).unwrap_or("?");
        let context: Vec<&str> = part
            .get("context")
            .and_then(|c| c.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        // 1. Every context path must resolve.
        for c in &context {
            if !Path::new(c).exists() {
                problems.insert(format!("{name}: context path does not exist: {c}"));
            }
        }

        // 2. Every capability a part's world imports should have its signature
        //    quoted in the contract — the world is the app's own .wit, among
        //    the part's context.
        for w in context.iter().filter(|c| c.ends_with(".wit") && c.contains("-domain/")) {
            let Ok(text) = fs::read_to_string(w) else { continue };
            for ns in wit_imports(&text) {
                if ns == "wasi" || ns == "comp" {
                    continue;
                }
                let alias = contract_alias(&ns);
                let covered = contract.contains(&format!("{alias}::"))
                    || contract.contains(&format!("{ns}:"));
                if !covered {
                    problems.insert(format!(
                        "{name}: world imports `{ns}:` but the contract never quotes \
                         `{alias}::` — the part must guess the signature"
                    ));
                }
            }
        }
    }

    if problems.is_empty() {
        println!("OK  {title}");
        Ok(true)
    } else {
        println!("FAIL {title}");
        for p in &problems {
            println!("  · {p}");
        }
        Ok(false)
    }
}

pub(crate) fn contract_critic(goals: &[PathBuf]) -> Result<()> {
    if goals.is_empty() {
        anyhow::bail!("usage: cargo xtask contract-critic <goal.toml> [<goal.toml> ...]");
    }
    let mut all_ok = true;
    for g in goals {
        if !contract_critic_one(g)? {
            all_ok = false;
        }
    }
    if all_ok {
        Ok(())
    } else {
        anyhow::bail!("one or more goals have contract problems");
    }
}
