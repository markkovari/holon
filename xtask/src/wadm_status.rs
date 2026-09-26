//! `cargo xtask wadm-status` — was tools/wadm-status.py.
//!
//! wadm's own status JSON is nested and verbose enough that reading it raw is
//! how a real failure gets missed. Reads `wadm.api.<lattice>.model.status.<app>`
//! on stdin, prints one line per scaler and the reason when it failed.

use anyhow::{Context, Result};
use std::io::Read;

pub(crate) fn wadm_status() -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).context("reading stdin")?;
    let v: serde_json::Value = serde_json::from_str(&input).context("parsing wadm status JSON")?;
    let st = v.get("status").unwrap_or(&v);
    let scalers = st.get("scalers").and_then(|s| s.as_array()).cloned().unwrap_or_default();

    if scalers.is_empty() {
        // wadm ignores a trait type it does not understand rather than
        // refusing it, so "no scalers" is what a v2 manifest on a v1 wadm
        // looks like: deployed, running nothing. Worth saying out loud.
        println!("  no scalers — if this manifest was rendered --api v2, this wadm ignored it");
    }
    for s in &scalers {
        let kind = s.get("kind").and_then(|k| k.as_str()).unwrap_or("?");
        let name: String =
            s.get("name").and_then(|n| n.as_str()).unwrap_or("?").chars().take(46).collect();
        let status_type = s.pointer("/status/type").and_then(|t| t.as_str()).unwrap_or("?");
        println!("  {kind:<12} {name:<46} {status_type}");
        if status_type == "failed" {
            let msg: String = s
                .pointer("/status/message")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .chars()
                .take(160)
                .collect();
            println!("               {msg}");
        }
    }
    Ok(())
}
