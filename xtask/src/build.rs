//! `cargo xtask build` — build every wasm32-wasip2 component and stamp its
//! metadata, so `comp-plug`'s catalogue and the studio can name it.

use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use crate::util::run_cmd;

fn get_rustc_version() -> Result<String> {
    let output = Command::new("rustc").arg("--version").output()?;
    let s = String::from_utf8_lossy(&output.stdout);
    let ver = s.split_whitespace().nth(1).unwrap_or("unknown").to_string();
    Ok(ver)
}

fn has_newer_wit(dir: &Path, stamp_time: SystemTime) -> bool {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                if has_newer_wit(&path, stamp_time) {
                    return true;
                }
            } else if path.extension().is_some_and(|e| e == "wit") {
                if let Ok(meta) = path.metadata() {
                    if let Ok(mtime) = meta.modified() {
                        if mtime > stamp_time {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// FNV-1a 64 of an artifact, hex — stable across toolchains, unlike std's
/// `DefaultHasher`, so a rustc upgrade does not re-stamp everything.
fn content_hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

pub(crate) fn build_components(force: bool) -> Result<()> {
    println!("{}", "Building WASM components (wasm32-wasip2)...".cyan().bold());

    let marker_dir = PathBuf::from("components/target/.build-stamps");
    fs::create_dir_all(&marker_dir)?;
    let wit_checked = marker_dir.join(".wit-checked");

    let need_wit_check = if force || !wit_checked.exists() {
        true
    } else {
        let stamp_time = wit_checked.metadata()?.modified()?;
        has_newer_wit(Path::new("components"), stamp_time)
    };

    if need_wit_check {
        let mut cmd = Command::new("cargo");
        cmd.args(["component", "check", "--release"]).current_dir("components");
        run_cmd(&mut cmd, "cargo component check --release (WIT bindings)")?;
        fs::write(&wit_checked, b"")?;
    }

    let mut cmd = Command::new("cargo");
    cmd.args(["build", "--release", "--target", "wasm32-wasip2"]).current_dir("components");
    run_cmd(&mut cmd, "cargo build --release --target wasm32-wasip2")?;

    let rustv = get_rustc_version().unwrap_or_else(|_| "1.85.0".to_string());
    let mut stamped = 0;
    let mut skipped = 0;
    let mut pruned = 0;

    // Prune stale binaries from directories read by comp-plug
    let stale_dirs = [
        "components/target/wasm32-wasip2/debug",
        "components/target/wasm32-wasip1/release",
        "components/target/wasm32-wasip1/debug",
    ];
    let registered = comp_metadata::component::registered_components(Path::new("."));
    for dir in stale_dirs {
        let p = Path::new(dir);
        if p.is_dir() {
            if let Ok(entries) = fs::read_dir(p) {
                for entry in entries.flatten() {
                    let file_path = entry.path();
                    if file_path.extension().is_some_and(|e| e == "wasm") {
                        let stem = file_path.file_stem().unwrap().to_string_lossy();
                        let name = stem.replace('_', "-");
                        if !registered.contains(&name) {
                            let _ = fs::remove_file(&file_path);
                            pruned += 1;
                        }
                    }
                }
            }
        }
    }

    let wasip2_dir = Path::new("components/target/wasm32-wasip2/release");
    if wasip2_dir.is_dir() {
        for entry in fs::read_dir(wasip2_dir)?.flatten() {
            let file_path = entry.path();
            if file_path.extension().is_some_and(|e| e == "wasm") {
                let stem = file_path.file_stem().unwrap().to_string_lossy();
                let name = stem.replace('_', "-");
                let stamp = marker_dir.join(&name);

                if !registered.contains(&name) {
                    let _ = fs::remove_file(&file_path);
                    let _ = fs::remove_file(&stamp);
                    println!("pruned {} — components/{}/Cargo.toml is gone", name, name);
                    pruned += 1;
                    continue;
                }

                // Stale by CONTENT, not mtime. cargo uplifts `release/<x>.wasm`
                // from `deps/` (a hard link or copy), so any later `cargo build`
                // that touches the crate puts the UNNAMED artifact back — with the
                // deps file's old mtime, older than the stamp. An mtime check then
                // skipped it, and the studio saw a component named "". The stamp
                // holds a hash of the file as stamped; anything else is re-stamped.
                let bytes = fs::read(&file_path)
                    .with_context(|| format!("reading {}", file_path.display()))?;
                if !force
                    && fs::read_to_string(&stamp).is_ok_and(|h| h.trim() == content_hash(&bytes))
                {
                    skipped += 1;
                    continue;
                }

                // Stamp metadata
                let named_path = file_path.with_extension("named");
                let mut stamp_cmd = Command::new("wasm-tools");
                stamp_cmd.args([
                    "metadata",
                    "add",
                    "--name",
                    &name,
                    "--language",
                    &format!("Rust={rustv}"),
                    file_path.to_str().unwrap(),
                    "-o",
                    named_path.to_str().unwrap(),
                ]);
                run_cmd(&mut stamp_cmd, &format!("stamping {name} metadata"))?;
                fs::rename(&named_path, &file_path)?;
                // The same bytes into `deps/`, which is what cargo uplifts FROM:
                // otherwise every `cargo build` (this one included, next time)
                // links the unnamed artifact straight back over the stamp.
                // Written beside and renamed, so a hard link between the two is
                // replaced rather than written through. An output newer than its
                // sources leaves cargo's fingerprint fresh.
                let deps_path = wasip2_dir.join("deps").join(file_path.file_name().unwrap());
                if deps_path.is_file() {
                    let tmp = deps_path.with_extension("named");
                    fs::copy(&file_path, &tmp)?;
                    fs::rename(&tmp, &deps_path)?;
                }
                fs::write(&stamp, content_hash(&fs::read(&file_path)?))?;
                stamped += 1;
            }
        }
    }

    let total = stamped + skipped;
    println!(
        "{}",
        format!(
            "✔ Built {total} components (wasm32-wasip2, named) — stamped {stamped}, unchanged {skipped}, pruned {pruned}"
        )
        .green()
        .bold()
    );
    Ok(())
}
