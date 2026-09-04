use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

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

        /// Key-value storage backend (sqlite, memory, redis, nats)
        #[arg(long)]
        kv: Option<String>,
    },

    /// Fast type-checking across workspaces
    Check,

    /// Clean build artifacts, targets, and stamps
    Clean,

    /// List all registered applications in apps/
    List,
}

#[derive(Debug, serde::Deserialize)]
struct AppSpec {
    name: String,
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    artifact: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    static_dir: Option<String>,
    #[serde(default)]
    kv: Option<String>,
}

fn run_cmd(cmd: &mut Command, desc: &str) -> Result<()> {
    println!("{} {}", "→".cyan().bold(), desc.bold());
    let status = cmd.status().with_context(|| format!("Failed to execute {desc}"))?;
    if !status.success() {
        anyhow::bail!("Command failed with status: {status}");
    }
    Ok(())
}

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
                if path.file_name().map_or(false, |n| n == "target") {
                    continue;
                }
                if has_newer_wit(&path, stamp_time) {
                    return true;
                }
            } else if path.extension().map_or(false, |e| e == "wit") {
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

fn build_components(force: bool) -> Result<()> {
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
        cmd.args(["component", "check", "--release"])
            .current_dir("components");
        run_cmd(&mut cmd, "cargo component check --release (WIT bindings)")?;
        fs::write(&wit_checked, b"")?;
    }

    let mut cmd = Command::new("cargo");
    cmd.args(["build", "--release", "--target", "wasm32-wasip2"])
        .current_dir("components");
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
    for dir in stale_dirs {
        let p = Path::new(dir);
        if p.is_dir() {
            if let Ok(entries) = fs::read_dir(p) {
                for entry in entries.flatten() {
                    let file_path = entry.path();
                    if file_path.extension().map_or(false, |e| e == "wasm") {
                        let stem = file_path.file_stem().unwrap().to_string_lossy();
                        let name = stem.replace('_', "-");
                        let crate_cargo = PathBuf::from("components").join(&name).join("Cargo.toml");
                        if !crate_cargo.exists() {
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
            if file_path.extension().map_or(false, |e| e == "wasm") {
                let stem = file_path.file_stem().unwrap().to_string_lossy();
                let name = stem.replace('_', "-");
                let crate_cargo = PathBuf::from("components").join(&name).join("Cargo.toml");
                let stamp = marker_dir.join(&name);

                if !crate_cargo.exists() {
                    let _ = fs::remove_file(&file_path);
                    let _ = fs::remove_file(&stamp);
                    println!("pruned {} — components/{}/Cargo.toml is gone", name, name);
                    pruned += 1;
                    continue;
                }

                if !force && stamp.exists() {
                    if let (Ok(file_meta), Ok(stamp_meta)) = (file_path.metadata(), stamp.metadata()) {
                        if let (Ok(file_time), Ok(stamp_time)) = (file_meta.modified(), stamp_meta.modified()) {
                            if file_time <= stamp_time {
                                skipped += 1;
                                continue;
                            }
                        }
                    }
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
                fs::write(&stamp, b"")?;
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

fn compose_app(app: Option<&str>) -> Result<()> {
    // Ensure comp-plug binary is built
    let mut build_plug = Command::new("cargo");
    build_plug.args([
        "build",
        "--manifest-path",
        "reconciler/Cargo.toml",
        "--release",
        "--bin",
        "comp-plug",
    ]);
    run_cmd(&mut build_plug, "build comp-plug tool")?;

    let comp_plug_bin = PathBuf::from("reconciler/target/release/comp-plug");

    match app {
        None => {
            println!("{}", "Composing default auth-guard...".cyan().bold());
            fs::create_dir_all("components/target")?;
            let output = Command::new(&comp_plug_bin).arg("auth-guard").output()?;
            if !output.status.success() {
                anyhow::bail!("comp-plug auth-guard failed: {}", String::from_utf8_lossy(&output.stderr));
            }
            let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let dest = "components/target/auth_guard.composed.wasm";
            fs::copy(&path_str, dest)?;
            println!("{} Composed auth-guard -> {}", "✔".green(), dest);
        }
        Some("grocery") => {
            println!("{}", "Composing grocery-domain application...".cyan().bold());
            // Build UI
            if Path::new("examples/grocery/ui/package.json").exists() {
                let mut npm = Command::new("npm");
                npm.args(["--prefix", "examples/grocery/ui", "run", "build"]);
                run_cmd(&mut npm, "npm run build (grocery UI)")?;
            }

            // Build components
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
            run_cmd(&mut cargo_wasm, "build grocery components")?;

            fs::create_dir_all("components/target/grocery-override")?;
            let src_assets = "components/target/wasm32-wasip2/release/grocery_assets.wasm";
            if Path::new(src_assets).exists() {
                fs::copy(src_assets, "components/target/grocery-override/grocery_assets.wasm")?;
            }

            let mut plug = Command::new(&comp_plug_bin);
            plug.args([
                "grocery-domain",
                "--dir",
                "components/target/grocery-override",
                "--out",
                "components/target/composed",
            ]);
            run_cmd(&mut plug, "comp-plug grocery-domain")?;

            // Copy to canonical target artifact location
            let canonical = "components/target/grocery_domain.composed.wasm";
            if let Ok(entries) = fs::read_dir("components/target/composed") {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.file_name().map_or(false, |n| n.to_string_lossy().starts_with("grocery-domain.")) {
                        fs::copy(&p, canonical)?;
                        break;
                    }
                }
            }
            println!("{} Composed grocery-domain -> {}", "✔".green(), canonical);
        }
        Some(name) => {
            println!("{}", format!("Composing {name}...").cyan().bold());

            // Check if UI build exists
            let ui_pkg = format!("examples/{name}/ui/package.json");
            if Path::new(&ui_pkg).exists() {
                let mut npm = Command::new("npm");
                npm.args(["--prefix", &format!("examples/{name}/ui"), "run", "build"]);
                run_cmd(&mut npm, &format!("npm run build ({name} UI)"))?;
            }

            let domain_name = if name.ends_with("-domain") {
                name.to_string()
            } else {
                format!("{name}-domain")
            };

            fs::create_dir_all("components/target")?;
            let output = Command::new(&comp_plug_bin).arg(&domain_name).output()?;
            if !output.status.success() {
                anyhow::bail!("comp-plug {domain_name} failed: {}", String::from_utf8_lossy(&output.stderr));
            }
            let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();

            // Destination artifact
            let spec_path = format!("apps/{name}.toml");
            let dest = if let Ok(spec_bytes) = fs::read_to_string(&spec_path) {
                if let Ok(spec) = toml::from_str::<AppSpec>(&spec_bytes) {
                    spec.artifact.unwrap_or_else(|| format!("components/target/{name}.composed.wasm"))
                } else {
                    format!("components/target/{name}.composed.wasm")
                }
            } else {
                format!("components/target/{name}.composed.wasm")
            };

            if let Some(parent) = Path::new(&dest).parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&path_str, &dest)?;
            println!("{} Composed {} -> {}", "✔".green(), name, dest);
        }
    }
    Ok(())
}

fn host_app(app: &str, addr: Option<&str>, kv: Option<&str>) -> Result<()> {
    // Read spec if available
    let spec_path = format!("apps/{app}.toml");
    let (artifact_path, default_port, default_kv, static_dir) = if let Ok(content) = fs::read_to_string(&spec_path) {
        let spec: AppSpec = toml::from_str(&content).context("Parsing app spec")?;
        (
            spec.artifact.unwrap_or_else(|| format!("components/target/{app}_domain.composed.wasm")),
            spec.port.unwrap_or(3055),
            spec.kv.unwrap_or_else(|| "sqlite".to_string()),
            spec.static_dir,
        )
    } else {
        (format!("components/target/{app}_domain.composed.wasm"), 3055, "sqlite".to_string(), None)
    };

    if !Path::new(&artifact_path).exists() {
        println!("{}", format!("Artifact {artifact_path} not found. Composing {app} first...").yellow());
        compose_app(Some(app))?;
    }

    // Ensure comp-host is built
    let mut build_host = Command::new("cargo");
    build_host.args(["build", "--manifest-path", "host/Cargo.toml", "--release", "--bin", "comp-host"]);
    run_cmd(&mut build_host, "build comp-host")?;

    let bind_addr = addr.map(|a| a.to_string()).unwrap_or_else(|| format!("0.0.0.0:{default_port}"));
    let kv_mode = kv.map(|k| k.to_string()).unwrap_or(default_kv);

    println!("{}", format!("Starting {app} on http://{bind_addr} (kv: {kv_mode})...").green().bold());

    let mut host_cmd = Command::new("host/target/release/comp-host");
    host_cmd.args([
        "--app",
        app,
        "--component",
        &artifact_path,
        "--addr",
        &bind_addr,
        "--config",
        &format!("default-tenant={app}"),
    ]);

    if let Some(dir) = static_dir {
        if Path::new(&dir).exists() {
            host_cmd.args(["--static-dir", &dir]);
        }
    }

    let status = host_cmd.status().context("Failed to run comp-host")?;
    if !status.success() {
        anyhow::bail!("comp-host exited with status: {status}");
    }

    Ok(())
}

fn list_apps() -> Result<()> {
    println!("{}", "Registered Holon Applications (apps/*.toml):".cyan().bold());
    println!("{:<20} {:<8} {:<10} {:<30}", "APP", "PORT", "KV", "DOMAIN");
    println!("{:-<20} {:-<8} {:-<10} {:-<30}", "", "", "", "");

    let mut apps = Vec::new();
    if let Ok(entries) = fs::read_dir("apps") {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().map_or(false, |e| e == "toml") {
                if let Ok(content) = fs::read_to_string(&p) {
                    if let Ok(spec) = toml::from_str::<AppSpec>(&content) {
                        apps.push(spec);
                    }
                }
            }
        }
    }

    apps.sort_by(|a, b| a.name.cmp(&b.name));
    for a in apps {
        let port_str = a.port.map_or("-".to_string(), |p| p.to_string());
        let kv_str = a.kv.unwrap_or_else(|| "-".to_string());
        let domain_str = a.domain.unwrap_or_else(|| "-".to_string());
        println!("{:<20} {:<8} {:<10} {:<30}", a.name, port_str, kv_str, domain_str);
    }

    Ok(())
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
            compose_app(app.as_deref())?;
        }

        Commands::Host { app, addr, kv } => {
            host_app(&app, addr.as_deref(), kv.as_deref())?;
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
            println!("{}", "Cleaning targets and build stamps across all workspaces...".yellow().bold());
            let workspaces = ["components", "host", "lattice", "cli", "reconciler"];
            for ws in workspaces {
                let manifest = format!("{ws}/Cargo.toml");
                if Path::new(&manifest).exists() {
                    let mut cmd = Command::new("cargo");
                    cmd.args(["clean", "--manifest-path", &manifest]);
                    let _ = run_cmd(&mut cmd, &format!("cargo clean on {ws}"));
                }
            }
            let _ = fs::remove_dir_all("components/target");
            let _ = fs::remove_dir_all(".zig-cache");
            let _ = fs::remove_dir_all("zig-out");
            println!("{}", "✔ Cleaned all workspaces successfully!".green().bold());
        }

        Commands::List => {
            list_apps()?;
        }
    }

    Ok(())
}
