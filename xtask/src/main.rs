use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::path::PathBuf;
use std::process::Command;

mod build;
mod compose;
mod contract_critic;
mod e2e_cmd;
mod examples;
mod host;
mod util;
mod wadm_status;

use build::build_components;
use compose::{compose_app, list_apps};
use contract_critic::contract_critic;
use e2e_cmd::e2e;
use examples::stage_examples;
use host::{host_app, seed_studio};
use util::run_cmd;
use wadm_status::wadm_status;

#[derive(Parser)]
#[command(name = "cargo xtask")]
#[command(about = "Programmatic build and task runner for Holon", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build all WASM components (wasm32-wasip2) and stamp metadata
    Build {
        /// Force rebuild of all components and WIT bindings
        #[arg(long, short)]
        force: bool,
    },

    /// Run all workspace tests in parallel using cargo-nextest
    Test {
        /// Fast mode: skip long-running integration suites
        #[arg(long, short)]
        fast: bool,

        /// Additional filter expression passed to nextest
        #[arg(long, short = 'E')]
        filter: Option<String>,
    },

    /// Compose an application from its components via comp-plug
    Compose {
        /// Name of the application to compose (e.g. grocery, arena, console). If omitted, composes auth-guard.
        app: Option<String>,
    },

    /// Run an application on the native Rust host (comp-host)
    Host {
        /// Name of the application to run (e.g. grocery, arena, console)
        app: String,

        /// Address to bind to (defaults to port in apps/<app>.toml or 0.0.0.0:3055)
        #[arg(long)]
        addr: Option<String>,

        /// Key-value storage backend (memory, sqlite, redis, nats, surreal, turso).
        /// Defaults to `kv` in apps/<app>.toml, else sqlite
        #[arg(long)]
        kv: Option<String>,

        /// Extra `wasi:config` for this run only, repeatable: `--config key=value`.
        /// Passed to comp-host after the app's `[config]`, so it adds a key or
        /// overrides one without editing apps/<app>.toml — e.g. a test-only
        /// switch that must never be in the committed spec.
        #[arg(long = "config", value_name = "KEY=VALUE")]
        config: Vec<String>,
    },

    /// Stage the jco examples' `.wasm` inputs from the build (builds first)
    ///
    /// Every `jco transpile <x>.wasm` in examples/*/package.json gets its <x>.wasm
    /// copied (or composed) from what `cargo xtask build` produced — none of them
    /// are tracked.
    StageExamples,

    /// Run an app's end-to-end suite against its composed artifact on comp-host
    ///
    /// Builds, composes `<app>`, builds comp-host, then runs `cargo test --release`
    /// in examples/<app> (or its Playwright suite, for poll and console). Trailing
    /// args go to `cargo test`.
    ///
    /// Scripted variants, named after the recipes they replace:
    /// conformance-conduit, durable-saga, saga-golem, gate-golem, golem,
    /// gate-codec, and `binder-poly <lang> <capability>` (e.g. `binder-poly go
    /// portfolio-value`) — the binder's suite against a composition built in
    /// another language.
    E2e {
        /// App (an examples/<app> with a test suite) or a scripted variant
        target: String,

        /// Passed to `cargo test` (e.g. a test name); for binder-poly, <lang> <capability>
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Seed an already-running studio with every built component
    ///
    /// `cargo xtask host studio` does this itself once the host answers; this is
    /// for re-seeding after a rebuild without restarting it.
    SeedStudio {
        /// Where the studio is listening
        #[arg(long, default_value = "127.0.0.1:3054")]
        addr: String,
    },

    /// Fast type-checking across workspaces
    Check,

    /// Clean build artifacts, targets, and stamps
    Clean,

    /// List all registered applications in apps/
    List,

    /// Check a goal's contract against what its parts' worlds actually import —
    /// before a run spends a generation on a claim the components don't support.
    ContractCritic {
        /// Goal TOML file(s)
        goals: Vec<PathBuf>,
    },

    /// Pretty-print wadm scaler status JSON, one line per scaler with the
    /// reason when it failed. Reads a `wadm.api.<lattice>.model.status.<app>`
    /// reply on stdin.
    WadmStatus,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Build { force } => {
            build_components(force)?;
        }

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

        Commands::Compose { app } => {
            compose_app(app.as_deref(), true)?;
        }

        Commands::Host { app, addr, kv, config } => {
            host_app(&app, addr.as_deref(), kv.as_deref(), &config)?;
        }

        Commands::StageExamples => {
            stage_examples()?;
        }

        Commands::E2e { target, args } => {
            e2e(&target, &args)?;
        }

        Commands::SeedStudio { addr } => {
            seed_studio(&addr)?;
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

        Commands::Clean => {
            println!(
                "{}",
                "Cleaning targets and build stamps across all workspaces...".yellow().bold()
            );
            let workspaces = ["components", "host", "lattice", "cli", "reconciler"];
            for ws in workspaces {
                let manifest = format!("{ws}/Cargo.toml");
                if std::path::Path::new(&manifest).exists() {
                    let mut cmd = Command::new("cargo");
                    cmd.args(["clean", "--manifest-path", &manifest]);
                    let _ = run_cmd(&mut cmd, &format!("cargo clean on {ws}"));
                }
            }
            let _ = std::fs::remove_dir_all("components/target");
            let _ = std::fs::remove_dir_all(".zig-cache");
            let _ = std::fs::remove_dir_all("zig-out");
            println!("{}", "✔ Cleaned all workspaces successfully!".green().bold());
        }

        Commands::List => {
            list_apps()?;
        }

        Commands::ContractCritic { goals } => {
            contract_critic(&goals)?;
        }

        Commands::WadmStatus => {
            wadm_status()?;
        }
    }

    Ok(())
}
