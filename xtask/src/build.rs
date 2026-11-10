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

/// `cargo xtask bindings`: generate every p2 crate's `src/bindings.rs`.
///
/// cargo-component generates p2 bindings, and treats any crate with a `wit/` as
/// its job — including a p3 crate, whose bindings come from
/// `wit_bindgen::generate!` and whose world it cannot resolve. Its `--exclude` is
/// applied too late to help, so every p2 member is named instead. This replaces a
/// bare `cargo component check` anywhere one was run over the workspace.
pub(crate) fn generate_p2_bindings(exclude: &[String]) -> Result<()> {
    let mut cmd = Command::new("cargo");
    cmd.args(["component", "check", "--release"]).current_dir("components");
    for name in p2_members()?.iter().filter(|n| !exclude.contains(n)) {
        cmd.args(["-p", name]);
    }
    run_cmd(&mut cmd, "cargo component check --release (p2 WIT bindings)")
}

/// The components workspace's members, minus p3 crates — the ones that depend on
/// `wit-bindgen` itself rather than cargo-component's `wit-bindgen-rt`.
pub(crate) fn p2_members() -> Result<Vec<String>> {
    let out = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir("components")
        .output()
        .context("cargo metadata")?;
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let packages = meta["packages"].as_array().context("cargo metadata: no packages")?;
    Ok(packages
        .iter()
        .filter(|p| {
            !p["dependencies"]
                .as_array()
                .is_some_and(|d| d.iter().any(|d| d["name"] == "wit-bindgen"))
        })
        .filter_map(|p| p["name"].as_str().map(str::to_string))
        .collect())
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
        generate_p2_bindings(&[])?;
        fs::write(&wit_checked, b"")?;
    }

    let mut cmd = Command::new("cargo");
    cmd.args(["build", "--release", "--target", "wasm32-wasip2"]).current_dir("components");
    run_cmd(&mut cmd, "cargo build --release --target wasm32-wasip2")?;

    build_moonbit_components(force)?;

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
    let registered = holon_metadata::component::registered_components(Path::new("."));
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

                // Stale by CONTENT, not mtime: cargo uplifts `release/<x>.wasm` from
                // `deps/`, so a later `cargo build` puts the UNNAMED artifact back with
                // the deps file's old mtime, older than the stamp. The stamp holds a
                // hash of the file as stamped; anything else is re-stamped.
                let bytes = fs::read(&file_path)
                    .with_context(|| format!("reading {}", file_path.display()))?;
                if !force
                    && fs::read_to_string(&stamp).is_ok_and(|h| h.trim() == content_hash(&bytes))
                {
                    skipped += 1;
                    continue;
                }

                let is_mbt = Path::new("components").join(&name).join("moon.mod").is_file()
                    || Path::new("components").join(&name).join("moon.mod.json").is_file();
                let lang =
                    if is_mbt { "MoonBit=0.1.0".to_string() } else { format!("Rust={rustv}") };
                let named_path = file_path.with_extension("named");
                let mut stamp_cmd = Command::new("wasm-tools");
                stamp_cmd.args([
                    "metadata",
                    "add",
                    "--name",
                    &name,
                    "--language",
                    &lang,
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

fn find_moon() -> String {
    if let Ok(m) = std::env::var("MOON") {
        return m;
    }
    if Command::new("moon").arg("version").output().is_ok() {
        return "moon".to_string();
    }
    if let Ok(home) = std::env::var("HOME") {
        let p = PathBuf::from(home).join(".moon/bin/moon");
        if p.is_file() {
            return p.to_string_lossy().into_owned();
        }
    }
    "moon".to_string()
}

pub fn build_moonbit_components(_force: bool) -> Result<()> {
    let components_dir = Path::new("components");
    let target_dir = Path::new("components/target/wasm32-wasip2/release");
    fs::create_dir_all(target_dir)?;

    if let Ok(entries) = fs::read_dir(components_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir()
                && (path.join("moon.mod").is_file() || path.join("moon.mod.json").is_file())
            {
                let name = entry.file_name().to_string_lossy().into_owned();
                let snake = name.replace('-', "_");
                let out_wasm = target_dir.join(format!("{snake}.wasm"));

                println!("  Building MoonBit component: {}...", name.cyan());
                let moon = find_moon();
                let mut build = Command::new(&moon);
                build.args(["build", "--target", "wasm", "--release"]).current_dir(&path);
                run_cmd(&mut build, &format!("moon build {name}"))?;

                let core = path.join("_build/wasm/release/build/gen/gen.wasm");
                if !core.exists() {
                    continue;
                }

                let embedded = path.join("embedded.wasm");
                let wit_dir = if path.join("wit").is_dir() {
                    path.join("wit")
                } else {
                    PathBuf::from(format!("components/{name}/wit"))
                };

                let world = fs::read_dir(path.join("world"))
                    .ok()
                    .and_then(|entries| {
                        for e in entries.flatten() {
                            if e.path().is_dir() {
                                return Some(e.file_name().to_string_lossy().into_owned());
                            }
                        }
                        None
                    })
                    .or_else(|| {
                        fs::read_dir(&wit_dir).ok().and_then(|entries| {
                            for e in entries.flatten() {
                                if e.path().extension().is_some_and(|ext| ext == "wit") {
                                    if let Ok(w) = fs::read_to_string(e.path()) {
                                        if let Some(wname) = w.lines().find_map(|l| {
                                            l.trim()
                                                .strip_prefix("world ")
                                                .and_then(|r| r.split_whitespace().next())
                                                .map(|r| r.trim_end_matches(['{', ' ']).to_string())
                                        }) {
                                            return Some(wname);
                                        }
                                    }
                                }
                            }
                            None
                        })
                    })
                    .unwrap_or_else(|| name.clone());

                let mut embed = Command::new("wasm-tools");
                embed.args([
                    "component",
                    "embed",
                    wit_dir.to_str().unwrap(),
                    core.to_str().unwrap(),
                    "--world",
                    &world,
                    "--encoding",
                    "utf16",
                    "-o",
                    embedded.to_str().unwrap(),
                ]);
                run_cmd(&mut embed, "wasm-tools component embed")?;

                let mut new_comp = Command::new("wasm-tools");
                new_comp.args([
                    "component",
                    "new",
                    embedded.to_str().unwrap(),
                    "-o",
                    out_wasm.to_str().unwrap(),
                ]);
                run_cmd(&mut new_comp, "wasm-tools component new")?;
                let _ = fs::remove_file(&embedded);
            }
        }
    }
    Ok(())
}
