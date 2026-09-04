use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::process::Command;

#[derive(Parser)]
#[command(name = "cargo xtask")]
#[command(about = "Programmatic build and task runner for Holon", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run all workspace tests in parallel using cargo-nextest
    Test {
        /// Fast mode: skip long-running reconciler suites
        #[arg(long, short)]
        fast: bool,

        /// Additional filter expression passed to nextest
        #[arg(long, short = 'E')]
        filter: Option<String>,
    },

    /// Compose the grocery-domain application (UI + WASM + comp-plug)
    ComposeGrocery,

    /// Fast type-checking across workspaces
    Check,
}

fn run_cmd(cmd: &mut Command, desc: &str) -> Result<()> {
    println!("{} {}", "→".cyan().bold(), desc.bold());
    let status = cmd.status().with_context(|| format!("Failed to execute {desc}"))?;
    if !status.success() {
        anyhow::bail!("Command failed with status: {status}");
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Test { fast, filter } => {
            println!("{}", "Running Holon test suites via cargo-nextest...".green().bold());

            let mut workspaces = vec![
                ("cli", "cli/Cargo.toml", None),
                ("host", "host/Cargo.toml", None),
                ("lattice", "lattice/Cargo.toml", None),
            ];

            if !fast {
                workspaces.push(("reconciler (lib)", "reconciler/Cargo.toml", Some("--lib")));
            }

            for (name, manifest, extra_arg) in workspaces {
                let mut cmd = Command::new("cargo");
                cmd.args([
                    "nextest",
                    "run",
                    "--config-file",
                    ".config/nextest.toml",
                    "--manifest-path",
                    manifest,
                ]);

                if let Some(arg) = extra_arg {
                    cmd.arg(arg);
                }

                if let Some(ref f) = filter {
                    cmd.arg("-E").arg(f);
                }

                run_cmd(&mut cmd, &format!("nextest on {name} ({manifest})"))?;
            }

            println!("{}", "✔ All test suites passed successfully!".green().bold());
        }

        Commands::ComposeGrocery => {
            println!("{}", "Programmatic build & composition for grocery-domain...".cyan().bold());

            // 1. Build React UI
            let mut npm_build = Command::new("npm");
            npm_build.args(["--prefix", "examples/grocery/ui", "run", "build"]);
            run_cmd(&mut npm_build, "npm run build (grocery UI)")?;

            // 2. Build WASM components
            let mut cargo_wasm = Command::new("cargo");
            cargo_wasm.args([
                "build",
                "--manifest-path",
                "components/Cargo.toml",
                "--release",
                "--target",
                "wasm32-wasip2",
                "-p",
                "grocery-assets",
                "-p",
                "grocery-domain",
                "-p",
                "barcode-read",
            ]);
            run_cmd(&mut cargo_wasm, "cargo build (grocery-domain, assets, barcode-read)")?;

            // 3. Compose using comp-plug
            std::fs::create_dir_all("components/target/grocery-override")?;
            std::fs::copy(
                "components/target/wasm32-wasip2/release/grocery_assets.wasm",
                "components/target/grocery-override/grocery_assets.wasm",
            )?;

            let mut comp_plug = Command::new("cargo");
            comp_plug.args([
                "run",
                "--manifest-path",
                "reconciler/Cargo.toml",
                "--release",
                "--bin",
                "comp-plug",
                "--",
                "grocery-domain",
                "--dir",
                "components/target/grocery-override",
                "--out",
                "components/target/composed",
            ]);
            run_cmd(&mut comp_plug, "comp-plug (grocery-domain composition)")?;

            println!("{}", "✔ Composed grocery-domain successfully!".green().bold());
        }

        Commands::Check => {
            println!("{}", "Running workspace checks...".yellow().bold());
            let mut cmd = Command::new("cargo");
            cmd.args([
                "check",
                "--manifest-path",
                "components/Cargo.toml",
                "--target",
                "wasm32-wasip2",
                "-p",
                "grocery-domain",
            ]);
            run_cmd(&mut cmd, "cargo check grocery-domain (wasm32-wasip2)")?;
            println!("{}", "✔ Workspace check passed!".green().bold());
        }
    }

    Ok(())
}
