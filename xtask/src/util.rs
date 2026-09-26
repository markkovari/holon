//! Small helpers every other module here reaches for.

use anyhow::{Context, Result};
use colored::Colorize;
use std::process::Command;

/// Where `cargo xtask build` puts release wasm32-wasip2 artifacts — what
/// `compose`, `host`, `stage-examples` and `e2e` all copy FROM.
pub(crate) const RELEASE: &str = "components/target/wasm32-wasip2/release";

pub(crate) fn run_cmd(cmd: &mut Command, desc: &str) -> Result<()> {
    println!("{} {}", "→".cyan().bold(), desc.bold());
    let status = cmd.status().with_context(|| format!("Failed to execute {desc}"))?;
    if !status.success() {
        anyhow::bail!("Command failed with status: {status}");
    }
    Ok(())
}
