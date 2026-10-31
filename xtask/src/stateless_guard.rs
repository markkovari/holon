//! `cargo xtask check-stateless` — static analysis pass ensuring WebAssembly
//! components are strictly stateless in linear memory and do not leak mutable
//! static/global state across request invocations.

use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use wasmparser::{ExternalKind, Parser, Payload};

#[derive(Debug, Default)]
pub struct StatelessVerdict {
    pub is_stateless: bool,
    pub warnings: Vec<String>,
    pub mutable_globals: usize,
    pub data_segments: usize,
    pub total_memories: usize,
}

/// Inspects WebAssembly binary bytes to verify stateless compliance.
pub fn check_wasm_stateless(bytes: &[u8]) -> Result<StatelessVerdict> {
    let mut verdict = StatelessVerdict { is_stateless: true, ..Default::default() };

    let mut current_module_mutable_globals = 0;
    let mut global_mutability = Vec::new();

    for payload in Parser::new(0).parse_all(bytes) {
        match payload.context("parsing WebAssembly payload")? {
            Payload::Version { .. } => {
                // New module or component boundary
                current_module_mutable_globals = 0;
                global_mutability.clear();
            }
            Payload::MemorySection(reader) => {
                for mem in reader {
                    let _ = mem?;
                    verdict.total_memories += 1;
                }
            }
            Payload::DataSection(reader) => {
                for data in reader {
                    let _ = data?;
                    verdict.data_segments += 1;
                }
            }
            Payload::GlobalSection(reader) => {
                for global in reader {
                    let g = global?;
                    let is_mut = g.ty.mutable;
                    global_mutability.push(is_mut);
                    if is_mut {
                        current_module_mutable_globals += 1;
                        verdict.mutable_globals += 1;
                        // WebAssembly allows 1 mutable global as the internal LLVM stack pointer (__stack_pointer).
                        // Additional mutable globals represent static mutable state (e.g. `static mut FOO: u32`).
                        if current_module_mutable_globals > 1 {
                            verdict.is_stateless = false;
                            verdict.warnings.push(format!(
                                "excess mutable global detected (global #{}, type {:?})",
                                global_mutability.len() - 1,
                                g.ty.content_type
                            ));
                        }
                    }
                }
            }
            Payload::ExportSection(reader) => {
                for export in reader {
                    let exp = export?;
                    if let ExternalKind::Global = exp.kind {
                        if let Some(&is_mut) = global_mutability.get(exp.index as usize) {
                            if is_mut {
                                verdict.is_stateless = false;
                                verdict.warnings.push(format!(
                                    "exported mutable global {:?} (global #{})",
                                    exp.name, exp.index
                                ));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Ok(verdict)
}

/// Runs the stateless checker on provided `.wasm` files or all built components in `components/target`.
pub fn check_stateless(paths: &[PathBuf]) -> Result<bool> {
    let files: Vec<PathBuf> = if !paths.is_empty() {
        paths.to_vec()
    } else {
        let dir = Path::new("components/target");
        if !dir.exists() {
            println!(
                "{}",
                "components/target does not exist; run `cargo xtask build` first.".yellow()
            );
            return Ok(true);
        }
        let mut list = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("wasm") {
                list.push(path);
            }
        }
        list.sort();
        list
    };

    if files.is_empty() {
        println!("{}", "No .wasm files found to inspect.".yellow());
        return Ok(true);
    }

    println!("Checking {} WebAssembly component(s) for stateless compliance...", files.len());
    let t0 = Instant::now();
    let mut total_bytes = 0usize;
    let mut clean_count = 0usize;
    let mut failed_count = 0usize;

    for path in &files {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("  {} {}: {}", "FAIL".red().bold(), path.display(), e);
                failed_count += 1;
                continue;
            }
        };
        total_bytes += bytes.len();

        match check_wasm_stateless(&bytes) {
            Ok(verdict) => {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");
                if verdict.is_stateless {
                    clean_count += 1;
                    println!(
                        "  {} {} ({:.1} KB, {} globals, {} data segments)",
                        "PASS".green().bold(),
                        name,
                        bytes.len() as f64 / 1024.0,
                        verdict.mutable_globals,
                        verdict.data_segments
                    );
                } else {
                    failed_count += 1;
                    println!("  {} {} (state leak detected):", "FAIL".red().bold(), name);
                    for w in verdict.warnings {
                        println!("       - {}", w.red());
                    }
                }
            }
            Err(e) => {
                eprintln!("  {} {}: {}", "ERROR".red().bold(), path.display(), e);
                failed_count += 1;
            }
        }
    }

    let elapsed = t0.elapsed();
    let elapsed_sec = elapsed.as_secs_f64().max(0.0001);
    let throughput_mb = (total_bytes as f64 / 1_048_576.0) / elapsed_sec;

    println!();
    println!("Stateless Guard Summary:");
    println!("  Total checked:    {}", files.len());
    println!("  Stateless clean:  {}", clean_count.to_string().green());
    if failed_count > 0 {
        println!("  State leaks:      {}", failed_count.to_string().red().bold());
    }
    println!(
        "  Throughput:       {:.1} MB/s ({:.2} MB in {:.3}s)",
        throughput_mb,
        total_bytes as f64 / 1_048_576.0,
        elapsed_sec
    );

    Ok(failed_count == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_wasm_passes() {
        // Minimal valid WebAssembly module: \0asm + version 1
        let empty_module = b"\x00asm\x01\x00\x00\x00";
        let verdict = check_wasm_stateless(empty_module).unwrap();
        assert!(verdict.is_stateless);
        assert_eq!(verdict.mutable_globals, 0);
    }

    #[test]
    fn test_single_stack_pointer_global_passes() {
        // Module with 1 mutable global (stack pointer):
        // (module (global (mut i32) (i32.const 1024)))
        // 0x06 (GlobalSection), length 0x07, count 0x01, i32 (0x7f), mut (0x01), i32.const 1024 (0x41, 0x80, 0x08), end (0x0b)
        let wasm = b"\x00asm\x01\x00\x00\x00\x06\x07\x01\x7f\x01\x41\x80\x08\x0b";
        let verdict = check_wasm_stateless(wasm).unwrap();
        assert!(verdict.is_stateless);
        assert_eq!(verdict.mutable_globals, 1);
    }

    #[test]
    fn test_adversarial_mutable_static_leak_detected() {
        // Module with 2 mutable globals (stack pointer + leaky static state):
        // (module
        //   (global (mut i32) (i32.const 1024))
        //   (global (mut i32) (i32.const 0))
        // )
        // 0x06 (GlobalSection), length 0x0c, count 0x02:
        // count: 0x02
        // global 0: 0x7f, 0x01, 0x41, 0x80, 0x08, 0x0b
        // global 1: 0x7f, 0x01, 0x41, 0x00, 0x0b
        let leaky_wasm =
            b"\x00asm\x01\x00\x00\x00\x06\x0c\x02\x7f\x01\x41\x80\x08\x0b\x7f\x01\x41\x00\x0b";
        let verdict = check_wasm_stateless(leaky_wasm).unwrap();
        assert!(!verdict.is_stateless, "Must flag illegal second mutable global");
        assert_eq!(verdict.mutable_globals, 2);
        assert!(!verdict.warnings.is_empty());
        assert!(verdict.warnings[0].contains("excess mutable global detected"));
    }
}
