//! `comp-plug` — a shim over [`comp_reconciler::plug`], for callers that are not
//! Rust.
//!
//! The composition itself is a library call and belongs in a library: the loop
//! composes a candidate in-process, with the wiring, the gaps and the failure as
//! values. This exists because a GATE is a shell script — `e2e-access.sh` has to
//! get a composed artifact from somewhere before it can start a host — and a shell
//! script cannot call a Rust function. It is deliberately thin: find the
//! components, ask the library, print the path. No decisions live here.
//!
//!   comp-plug clinic-domain --dir components/target/wasm32-wasip2/debug
//!   comp-plug clinic-domain --wiring     # what would be plugged, and what is missing

use std::path::PathBuf;

use clap::Parser;
use comp_reconciler::plug::{compose_to, default_dirs, wiring, Catalog};

#[derive(Parser)]
#[command(name = "comp-plug", about = "Compose a component with what it imports")]
struct Args {
    /// The component, by crate name (`clinic-domain`).
    component: String,

    /// Print what would be plugged and what nothing exports; compose nothing.
    #[arg(long)]
    wiring: bool,

    /// Where to look for built components. Repeatable; earlier wins, and the
    /// defaults are appended — so a gate that rebuilt one crate passes its own
    /// output and lets every plug resolve against `just build`'s.
    #[arg(long = "dir")]
    dirs: Vec<PathBuf>,

    /// Where composed artifacts are written.
    #[arg(long)]
    out: Option<PathBuf>,
}

fn main() -> Result<(), String> {
    let args = Args::parse();
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let dirs: Vec<PathBuf> = args.dirs.iter().cloned().chain(default_dirs(&root)).collect();

    let catalog = Catalog::scan(&dirs);
    if catalog.is_empty() {
        return Err(format!(
            "no built components under {} — run `just build`",
            dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", ")
        ));
    }

    let wiring = wiring(&args.component, &catalog)?;
    // Missing capabilities go to stderr either way: on a `--wiring` they are the
    // answer, and on a compose they are the explanation for whatever `wac` says
    // next. A composition that silently drops one is how a component reaches
    // production still importing something nothing provides.
    for (node, iface) in &wiring.missing {
        eprintln!("  ! nothing built exports {iface} (imported by {node})");
    }

    if args.wiring {
        println!("{} plugs: {}", args.component, wiring.plugs.join(", "));
        return Ok(());
    }

    let out_dir = args.out.unwrap_or_else(|| root.join("components/target/composed"));
    println!("{}", compose_to(&args.component, &catalog, &out_dir)?.display());
    Ok(())
}
