//! CLI over [`comp_fleet::plug`] for gates, which are shell scripts.

use std::path::PathBuf;

use clap::Parser;
use comp_fleet::plug::{compose_to, default_dirs, wiring, Catalog};

#[derive(Parser)]
#[command(name = "comp-plug", about = "Compose a component with what it imports")]
struct Args {
    /// The component, by crate name (`clinic-domain`).
    component: String,

    /// Print what would be plugged and what nothing exports; compose nothing.
    #[arg(long)]
    wiring: bool,

    /// Where to look for built components. Repeatable; earlier wins, defaults appended.
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
            "no built components under {} — run `cargo xtask build --force`",
            dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", ")
        ));
    }

    let wiring = wiring(&args.component, &catalog)?;
    // To stderr either way: with `--wiring` they are the answer, else they explain `wac`.
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
