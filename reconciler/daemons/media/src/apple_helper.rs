//! The Swift helper (`tools/media-apple`): Core Image develop, Metal
//! sharpness, Vision — used per job when set and the file exists.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::decode::Decoded;

#[derive(Deserialize)]
pub(crate) struct HelperRendition {
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Deserialize)]
pub(crate) struct HelperOut {
    #[serde(default)]
    pub(crate) develop: Option<String>,
    pub(crate) renditions: BTreeMap<String, HelperRendition>,
    #[serde(default)]
    pub(crate) sharpness: Option<Value>,
    #[serde(default)]
    pub(crate) vision: Option<Value>,
    #[serde(default)]
    pub(crate) timings_ms: BTreeMap<String, u64>,
}

pub(crate) async fn run_helper(
    helper: &Path,
    original: &Path,
    dec: &Decoded,
    dir: &Path,
) -> Result<HelperOut> {
    let green_path = dir.join("green.f32");
    let bytes: Vec<u8> = dec.green.iter().flat_map(|v| v.to_le_bytes()).collect();
    tokio::fs::write(&green_path, bytes).await?;
    let out = tokio::process::Command::new(helper)
        .arg("--original")
        .arg(original)
        .arg("--green")
        .arg(&green_path)
        .arg("--green-width")
        .arg(dec.gw.to_string())
        .arg("--green-height")
        .arg(dec.gh.to_string())
        .arg("--out")
        .arg(dir)
        .kill_on_drop(true)
        .output()
        .await?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.trim().is_empty() {
        eprintln!("comp-media-apple: {}", stderr.trim());
    }
    if !out.status.success() {
        bail!("helper exited {}", out.status);
    }
    serde_json::from_slice(&out.stdout).context("helper printed something that is not its JSON")
}
