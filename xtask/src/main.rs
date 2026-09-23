use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
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

        /// Key-value storage backend (memory, sqlite, redis, nats, surreal, turso).
        /// Defaults to `kv` in apps/<app>.toml, else sqlite
        #[arg(long)]
        kv: Option<String>,
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
    let registered = comp_metadata::component::registered_components(Path::new("."));
    for dir in stale_dirs {
        let p = Path::new(dir);
        if p.is_dir() {
            if let Ok(entries) = fs::read_dir(p) {
                for entry in entries.flatten() {
                    let file_path = entry.path();
                    if file_path.extension().map_or(false, |e| e == "wasm") {
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
            if file_path.extension().map_or(false, |e| e == "wasm") {
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
                let bytes = fs::read(&file_path).with_context(|| format!("reading {}", file_path.display()))?;
                if !force && fs::read_to_string(&stamp).is_ok_and(|h| h.trim() == content_hash(&bytes)) {
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

/// What `compose` hands comp-plug for an app, and where the result lands.
struct ResolvedApp {
    /// The component comp-plug composes from its own imports.
    root: String,
    /// `components/target/<file>` — the path `host` serves and the e2e tests read.
    artifact: String,
}

/// One answer to "which component is app X, and where does its composition go",
/// shared by `compose`, `host` and `e2e` — they each used to guess separately,
/// and disagreed: `compose` looked for a `<name>-domain` crate while `host`
/// looked for `<name>_domain.composed.wasm`, so `authgate` (root `mfa-authgate`)
/// failed to compose and `eshop` composed to a file `host` never looked for.
fn resolve_app(name: &str) -> ResolvedApp {
    // apps/<name>.toml first: its `artifact` (and `root`, if it sets one) are the
    // app's own statement of what it is. `discover_apps` resolves the root from
    // them the same way `holon` does, including the `-domain`-first rule below.
    if let Some(a) = comp_metadata::app::discover_apps(Path::new("."))
        .into_iter()
        .find(|a| a.name == name)
    {
        return ResolvedApp { root: a.root, artifact: format!("components/target/{}", a.artifact) };
    }

    // A handful of names predate the `<name>-domain` convention and have no
    // apps/*.toml to say so — the old Justfile recipes named the real crate
    // directly. Checked before the convention below, which would otherwise look
    // for a `<name>-domain` that never existed.
    let aliased = match name {
        "eshop" => Some("eshop-gateway"),
        "login" => Some("login-app"),
        "webhook" => Some("webhook-ingest"),
        "ai" => Some("ai-inference"),
        _ => None,
    };

    // Most apps' entry point is `<name>-domain` by convention — and that has to
    // stay the FIRST check: `fs-watcher`/`lan-scanner`/`image-optimizer` have
    // both a bare capability crate (exports only its own library interface,
    // nothing wasi:http can call) and a `-domain` sibling that wraps it over
    // HTTP. Picking the bare one the moment it exists would silently regress
    // those — it has to lose to `-domain` whenever `-domain` exists too. Only
    // fall through to the bare name for a component that IS its own entry point
    // with no `-domain` sibling at all (`intent-router`).
    let root = if let Some(a) = aliased {
        a.to_string()
    } else if name.ends_with("-domain") {
        name.to_string()
    } else if Path::new(&format!("components/{name}-domain")).is_dir() {
        format!("{name}-domain")
    } else if Path::new(&format!("components/{name}")).is_dir() {
        name.to_string()
    } else {
        format!("{name}-domain")
    };
    // Named after the root, the way `discover_apps` names the ones it finds by
    // their WIT and the way the old `_derive` recipes did (`eshop_gateway`,
    // `login_app`, `webhook_ingest`).
    let artifact = format!("components/target/{}.composed.wasm", root.replace('-', "_"));
    ResolvedApp { root, artifact }
}

/// `comp-plug <root>` — which composes `root` against its own imports — copied
/// to `dest`. The old `_derive` recipe, which every `compose-*` was one call of.
fn derive(comp_plug_bin: &Path, root: &str, dest: &str) -> Result<()> {
    let output = Command::new(comp_plug_bin).arg(root).output()?;
    if !output.status.success() {
        anyhow::bail!("comp-plug {root} failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if let Some(parent) = Path::new(dest).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&path_str, dest)?;
    println!("{} Composed {} -> {}", "✔".green(), root, dest);
    Ok(())
}

/// Which exporter an app composes against, where its closure imports an
/// interface more than one built component exports. comp-plug's catalogue gives
/// such an interface to the FIRST exporter in name order, so without a pin the
/// pick is whatever sorts first — and moves whenever somebody adds a component.
/// `jobs` got `golem-bridge` for `durable:workflow/orchestrator` that way, and
/// then tried to reach Golem on 127.0.0.1:9006 with every job left `queued`.
///
/// The ambiguous interfaces, and who exports them (as of this writing):
///   durable:workflow/orchestrator — golem-bridge, inproc-workflow
///   llm:inference/inference       — anthropic-provider, llm-inference, mock-provider, openai-provider
///   ui:assets/files               — console-assets, grocery-assets, static-assets, track-assets
///   jev:decision/decision         — jev-decision, mock-jev-provider, typesafe-provider
///   graph:fitness/evaluator       — checks-runner, mock-fitness
///   actor:entity/handler          — actor-entity, ticket-actor (nothing imports it)
///
/// Only apps whose pick is clearly wrong are pinned: each wants the offline,
/// in-process provider its e2e suite (or its apps/*.toml `components`) names.
/// `grocery` and `intent-router` pin theirs in their own `compose_app` arms.
fn app_plug_pins(app: &str) -> &'static [&'static str] {
    match app {
        // In-process durable workflow, not the Golem bridge.
        "jobs" => &["inproc-workflow"],
        // Its own UI bundle, and the deterministic mock behind ai-inference
        // (the pick was anthropic-provider, which needs a network key).
        "track" => &["track-assets", "llm-inference"],
        // The deterministic mock its suite's header names.
        "photosocial" => &["llm-inference"],
        // apps/jev-router.toml composes it with the mock provider.
        "jev-router" => &["mock-jev-provider"],
        _ => &[],
    }
}

/// [`derive`], with `pins` preferred over every other exporter of the same
/// interface: comp-plug's `--dir` is scanned before the release dir, so a dir
/// holding only the pinned artifacts decides the pick.
fn derive_pinned(comp_plug_bin: &Path, root: &str, dest: &str, pins: &[&str]) -> Result<()> {
    let pin_dir = PathBuf::from(format!("components/target/pin-{root}"));
    let _ = fs::remove_dir_all(&pin_dir);
    fs::create_dir_all(&pin_dir)?;
    for pin in pins {
        let file = format!("{}.wasm", pin.replace('-', "_"));
        fs::copy(format!("{RELEASE}/{file}"), pin_dir.join(&file))
            .with_context(|| format!("pinning {pin} for {root} — is it built?"))?;
    }
    let output = Command::new(comp_plug_bin).arg(root).arg("--dir").arg(&pin_dir).output()?;
    let _ = fs::remove_dir_all(&pin_dir);
    if !output.status.success() {
        anyhow::bail!("comp-plug {root} failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if let Some(parent) = Path::new(dest).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&path_str, dest)?;
    println!("{} Composed {} -> {} (pinned: {})", "✔".green(), root, dest, pins.join(", "));
    Ok(())
}

/// Builds comp-plug and returns its path.
fn comp_plug() -> Result<PathBuf> {
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
    Ok(PathBuf::from("reconciler/target/release/comp-plug"))
}

/// Builds comp-host — what every e2e suite spawns, from `host/target/release`.
fn comp_host() -> Result<()> {
    let mut build_host = Command::new("cargo");
    build_host.args(["build", "--manifest-path", "host/Cargo.toml", "--release", "--bin", "comp-host"]);
    run_cmd(&mut build_host, "build comp-host")
}

fn compose_app(app: Option<&str>, build_ui: bool) -> Result<()> {
    let comp_plug_bin = comp_plug()?;

    match app {
        None => {
            println!("{}", "Composing default auth-guard...".cyan().bold());
            derive(&comp_plug_bin, "auth-guard", "components/target/auth_guard.composed.wasm")?;
        }
        Some("eshop") => {
            // Six services and a gateway, not one app: examples/eshop/run-local.sh
            // serves every one of these, so composing only the gateway (which is
            // what `eshop` resolves to for `host`) left five missing. Identity
            // is the existing accounts-app, untouched.
            println!("{}", "Composing every eshop service...".cyan().bold());
            for (root, out) in [
                ("auth-guard", "auth_guard"),
                ("eshop-catalog", "eshop_catalog"),
                ("eshop-basket", "eshop_basket"),
                ("eshop-ordering", "eshop_ordering"),
                ("eshop-payment", "eshop_payment"),
                ("accounts-app", "eshop_identity"),
                ("eshop-gateway", "eshop_gateway"),
                ("event-pusher", "event_pusher"),
            ] {
                derive(&comp_plug_bin, root, &format!("components/target/{out}.composed.wasm"))?;
            }
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
        Some("intent-router") => {
            println!("{}", "Composing intent-router...".cyan().bold());

            // `intent-router` imports `llm:inference/inference`, and more than one
            // built component satisfies it — `anthropic-provider`,
            // `openai-provider`, `mock-provider`. Nothing in the generic path
            // below disambiguates that (grocery is the only other app that
            // needed to), so this pins it the same way grocery pins its
            // override: an override dir holding only the one candidate this
            // app composes against. `mock-provider` because this app is meant
            // to compose and run for free, offline, with no API key — see
            // apps/intent-router.toml's own comment. A deployment wanting a
            // real provider composes by hand against a dir holding that one
            // instead.
            let mut cargo_wasm = Command::new("cargo");
            cargo_wasm.args([
                "build",
                "--manifest-path",
                "components/Cargo.toml",
                "--release",
                "--target",
                "wasm32-wasip2",
                "-p",
                "intent-router",
                "-p",
                "mock-provider",
            ]);
            run_cmd(&mut cargo_wasm, "build intent-router + mock-provider")?;

            let override_dir = "components/target/intent-router-override";
            fs::create_dir_all(override_dir)?;
            fs::copy(
                "components/target/wasm32-wasip2/release/mock_provider.wasm",
                format!("{override_dir}/mock_provider.wasm"),
            )?;

            let output = Command::new(&comp_plug_bin)
                .args(["intent-router", "--dir", override_dir])
                .output()?;
            if !output.status.success() {
                anyhow::bail!("comp-plug intent-router failed: {}", String::from_utf8_lossy(&output.stderr));
            }
            let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let dest = "components/target/intent_router.composed.wasm";
            fs::copy(&path_str, dest)?;
            println!("{} Composed intent-router -> {}", "✔".green(), dest);
        }
        Some(name) => {
            println!("{}", format!("Composing {name}...").cyan().bold());

            // Check if UI build exists. `e2e` skips it, as the old `e2e-*`
            // recipes did: the tests drive the API, not the SPA.
            let ui_pkg = format!("examples/{name}/ui/package.json");
            if build_ui && Path::new(&ui_pkg).exists() {
                // A fresh checkout has no node_modules, and `vite build` then
                // fails as `vite: command not found` — the old `build-<app>-ui`
                // recipes ran `npm ci` first for exactly this.
                if !Path::new(&format!("examples/{name}/ui/node_modules")).is_dir() {
                    let mut ci = Command::new("npm");
                    ci.args(["--prefix", &format!("examples/{name}/ui"), "ci"]);
                    run_cmd(&mut ci, &format!("npm ci ({name} UI)"))?;
                }
                let mut npm = Command::new("npm");
                npm.args(["--prefix", &format!("examples/{name}/ui"), "run", "build"]);
                run_cmd(&mut npm, &format!("npm run build ({name} UI)"))?;
            }

            let ResolvedApp { root: domain_name, artifact: dest } = resolve_app(name);

            let pins = app_plug_pins(name);
            if pins.is_empty() {
                derive(&comp_plug_bin, &domain_name, &dest)?;
            } else {
                derive_pinned(&comp_plug_bin, &domain_name, &dest, pins)?;
            }
        }
    }
    Ok(())
}

/// Kills every daemon this started when `host_app` returns, success or not —
/// otherwise a `cargo xtask host` a developer Ctrl-C'd leaves the daemon
/// bound to its port, and the next run fails to bind it.
struct DaemonGuard(Vec<std::process::Child>);
impl Drop for DaemonGuard {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
        }
    }
}

/// Waits up to 20s for something to accept connections on `addr`.
fn wait_for_listener(addr: &str) -> Result<()> {
    for _ in 0..100 {
        if std::net::TcpStream::connect(addr).is_ok() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    anyhow::bail!("nothing listening on {addr} after 20s")
}

/// Feeds the studio every component in this repo, by POSTing the built `.wasm`
/// artifacts to `/api/components?id=<name>`. Re-running is safe: an upload with
/// the same id replaces it. Was the `seed-studio` recipe; `curl` as it was,
/// rather than an HTTP client dependency for one loop.
fn seed_studio(addr: &str) -> Result<()> {
    let mut wasm: Vec<PathBuf> = fs::read_dir(RELEASE)
        .with_context(|| format!("reading {RELEASE} — run `cargo xtask build` first"))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "wasm"))
        .collect();
    wasm.sort();
    let (mut ok, mut skipped) = (0, 0);
    for f in &wasm {
        let name = f.file_stem().unwrap().to_string_lossy().replace('_', "-");
        let out = Command::new("curl")
            .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "-X", "POST", "--data-binary"])
            .arg(format!("@{}", f.display()))
            .args(["-H", "content-type: application/wasm"])
            .arg(format!("http://{addr}/api/components?id={name}"))
            .output()
            .context("running curl")?;
        let code = String::from_utf8_lossy(&out.stdout).to_string();
        if code == "201" {
            ok += 1;
        } else {
            skipped += 1;
            println!("  {code} {name}");
        }
    }
    println!("{} seeded {ok} components ({skipped} not accepted)", "✔".green());
    Ok(())
}

fn host_app(app: &str, addr: Option<&str>, kv: Option<&str>) -> Result<()> {
    // The same path `compose` writes, so `compose X && host X` finds it.
    let artifact_path = resolve_app(app).artifact;

    // Check spec for port/kv/static_dir
    let specs = comp_metadata::app::registered_apps(Path::new("."));
    let app_spec = specs.iter().find(|s| s.name == app);
    let default_port = app_spec.and_then(|s| s.port).unwrap_or(3055);
    let default_kv = app_spec.and_then(|s| s.kv_as_string()).unwrap_or_else(|| "sqlite".to_string());

    if !Path::new(&artifact_path).exists() {
        println!("{}", format!("Artifact {artifact_path} not found. Composing {app} first...").yellow());
        compose_app(Some(app), true)?;
    }

    comp_host()?;

    // Start every daemon this app declares — `cargo xtask host` used to leave
    // this to a second terminal, silently unusable for any of the twelve
    // ADR-0095 capabilities until someone remembered to run the daemon by
    // hand.
    let daemons = app_spec.map(|s| s.daemons.as_slice()).unwrap_or_default();
    let mut running = Vec::new();
    for d in daemons {
        let mut build = Command::new("cargo");
        build.args([
            "build",
            "--manifest-path",
            "reconciler/Cargo.toml",
            "--release",
            "--bin",
            &format!("comp-{}", d.name),
        ]);
        run_cmd(&mut build, &format!("build comp-{}", d.name))?;

        let mut cmd = Command::new(format!("reconciler/target/release/comp-{}", d.name));
        cmd.args(["--addr", &d.addr]);
        if let Some(flag) = &d.allow_flag {
            for v in &d.allow {
                cmd.args([format!("--{flag}"), v.clone()]);
            }
        }
        for a in &d.extra_args {
            for part in a.split_whitespace() {
                cmd.arg(part);
            }
        }
        if let Some(t) = &d.token {
            cmd.args(["--token", t]);
        }
        println!("{}", format!("  + daemon: comp-{} on {}", d.name, d.addr).cyan());
        running.push(cmd.spawn().with_context(|| format!("spawning comp-{}", d.name))?);
    }
    let _guard = DaemonGuard(running);

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

    // The `<name>-url`/`<name>-token` config a daemon-backed component reads,
    // and the egress allow-list to actually reach it — comp-host denies all
    // outbound HTTP by default, so a component and its daemon both running
    // was still an `unavailable` without this.
    for d in daemons {
        host_cmd.args(["--config", &format!("{}-url=http://{}", d.name, d.addr)]);
        if let Some(t) = &d.token {
            host_cmd.args(["--config", &format!("{}-token={t}", d.name)]);
        }
        host_cmd.args(["--egress", &d.addr, "--allow-private-egress"]);
    }

    if let Some(dir) = app_spec.and_then(|s| s.static_dir_as_string()) {
        if Path::new(&dir).exists() {
            host_cmd.args(["--static-dir", &dir]);
        }
    }

    let mut host = host_cmd.spawn().context("Failed to run comp-host")?;

    // The studio reflects components it is FED — a component cannot read the
    // filesystem (the host preopens no directories), so without this its palette
    // is empty. The old `host-studio` recipe seeded it once the host answered.
    if app == "studio" {
        let port = bind_addr.rsplit(':').next().unwrap_or("3054");
        let local = format!("127.0.0.1:{port}");
        if let Err(e) = wait_for_listener(&local).and_then(|()| seed_studio(&local)) {
            let _ = host.kill();
            return Err(e);
        }
        println!("{}", format!("studio on http://{local}").green().bold());
    }

    let status = host.wait().context("Failed to wait on comp-host")?;
    if !status.success() {
        anyhow::bail!("comp-host exited with status: {status}");
    }

    Ok(())
}

// ---- stage-examples: was `just examples-stage` ------------------------------
//
// 58 of the jco examples' `.wasm` inputs were TRACKED, and all 50 with a
// same-named component had drifted from it — not by a metadata stamp, by
// thousands of bytes. So ~40 examples were exercising frozen components that no
// longer existed anywhere else, and a green example said nothing about the
// component it claimed to demonstrate. They are build outputs now (`**/*.wasm`
// in .gitignore, and reconciler/tests/derived.rs refuses a tracked one), and
// this is what writes them.
//
// Derived from each example's own `package.json` rather than listed here: a
// hand-kept list of 58 is wrong the first time somebody adds an example, and
// wrong silently.

const RELEASE: &str = "components/target/wasm32-wasip2/release";

/// Three examples ask for a bare name and need the COMPOSED artifact, because a
/// bare component leaves non-WASI imports for jco to emit as bare specifiers,
/// which Node rejects outright as a URL scheme (`protocol 'audit:'`). auth-guard
/// imports ratelimit:guard + audit:log/recorder, and both it and audit-log
/// import audit:log/types — a TYPES-ONLY interface nothing exports, so
/// composition cannot satisfy it either; that one is stubbed at transpile time
/// by the shims the package.json files point at.
fn wants_composed(stem: &str) -> bool {
    stem.ends_with(".composed") || matches!(stem, "audit_log" | "auth_guard" | "webhook_ingest")
}

/// An example that demonstrates a component in-process needs a DETERMINISTIC,
/// offline composition. Where the interface has several exporters, say which —
/// otherwise the pick is alphabetical and moves whenever somebody adds one.
/// `llm:inference/inference` has four, and the pick once moved to
/// `anthropic-provider`: jco-ai went from self-contained to needing a network
/// key and failed on an unresolvable `comp:secrets/reader`.
fn preferred_plug(base: &str) -> Option<&'static str> {
    match base {
        "ai_inference" => Some("llm_inference"),
        _ => None,
    }
}

/// Components whose crate name is not the name the example uses.
fn bare_alias(stem: &str) -> String {
    match stem {
        "eventbus" => "event_bus".to_string(),
        "lock" => "lock_mutex".to_string(),
        "timer" => "scheduler_timer".to_string(),
        other => other.replace('-', "_"),
    }
}

/// Every `<x>.wasm` a `jco transpile <x>.wasm` in this package.json's scripts reads.
fn jco_inputs(package_json: &Path) -> Result<BTreeSet<String>> {
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(package_json)?)
        .with_context(|| format!("parsing {}", package_json.display()))?;
    let mut out = BTreeSet::new();
    let Some(scripts) = v.get("scripts").and_then(|s| s.as_object()) else { return Ok(out) };
    for script in scripts.values().filter_map(|s| s.as_str()) {
        let words: Vec<&str> = script.split_whitespace().collect();
        for w in words.windows(3) {
            if w[0] == "jco" && w[1] == "transpile" && w[2].ends_with(".wasm") {
                out.insert(w[2].to_string());
            }
        }
    }
    Ok(out)
}

fn stage_examples() -> Result<()> {
    build_components(false)?;
    let plug = comp_plug()?;

    let mut examples: Vec<PathBuf> = fs::read_dir("examples")?
        .flatten()
        .map(|e| e.path().join("package.json"))
        .filter(|p| p.is_file())
        .collect();
    examples.sort();

    // jco-vet-clinic wants auth_guard composed too; compose each one once.
    let mut composed: std::collections::BTreeMap<String, String> = Default::default();
    let mut staged = 0;
    for pj in &examples {
        let dir = pj.parent().unwrap();
        for want in jco_inputs(pj)? {
            let stem = want.strip_suffix(".wasm").unwrap();
            let src = if wants_composed(stem) {
                // A component composed against its own imports, so the component
                // name is the artifact name with the suffix off and underscores
                // hyphenated.
                let base = stem.strip_suffix(".composed").unwrap_or(stem);
                if let Some(p) = composed.get(base) {
                    p.clone()
                } else {
                    let out = format!("components/target/{base}.composed.wasm");
                    let root = base.replace('_', "-");
                    if let Some(pin) = preferred_plug(base) {
                        // `--dir` wins over the release dir, so a directory holding
                        // one artifact pins which exporter comp-plug picks.
                        let pin_dir = PathBuf::from(format!("components/target/stage-pin-{base}"));
                        let _ = fs::remove_dir_all(&pin_dir);
                        fs::create_dir_all(&pin_dir)?;
                        fs::copy(format!("{RELEASE}/{pin}.wasm"), pin_dir.join(format!("{pin}.wasm")))?;
                        let output = Command::new(&plug).arg("--dir").arg(&pin_dir).arg(&root).output()?;
                        let _ = fs::remove_dir_all(&pin_dir);
                        if !output.status.success() {
                            anyhow::bail!("comp-plug {root} failed: {}", String::from_utf8_lossy(&output.stderr));
                        }
                        fs::copy(String::from_utf8_lossy(&output.stdout).trim(), &out)?;
                    } else {
                        derive(&plug, &root, &out)?;
                    }
                    composed.insert(base.to_string(), out.clone());
                    out
                }
            } else {
                format!("{RELEASE}/{}.wasm", bare_alias(stem))
            };
            fs::copy(&src, dir.join(&want))
                .with_context(|| format!("staging {src} -> {}/{want}", dir.display()))?;
            staged += 1;
        }
    }
    println!(
        "{}",
        format!("✔ Staged {staged} example input(s) from the build — none of them tracked").green().bold()
    );
    Ok(())
}

// ---- e2e: was the `e2e-<app>` recipes (just/e2e.just) ------------------------
//
// These drive a real fleet — a host, a composed artifact — and assert what it
// serves. Almost every one was the same three lines (compose, build comp-host,
// `cargo test --release` in the example), so they are one rule here; the
// handful that ran a script instead are named.

fn run_script(script: &str) -> Result<()> {
    let mut cmd = Command::new("bash");
    cmd.arg(script);
    run_cmd(&mut cmd, &format!("bash {script}"))
}

fn e2e(target: &str, args: &[String]) -> Result<()> {
    match target {
        // RealWorld conformance (docs/apps/CONDUIT.md rung 4): the official Hurl
        // suite against the composed app. Needs `hurl`.
        "conformance-conduit" => {
            build_components(false)?;
            compose_app(Some("conduit"), false)?;
            comp_host()?;
            run_script("examples/conduit/conformance/run.sh")
        }
        // Durability proof (docs/apps/SAGA.md rung 3): kill the host mid-saga,
        // restart, show it resumes. Needs NATS on :4222.
        "durable-saga" => {
            build_components(false)?;
            compose_app(Some("saga"), false)?;
            comp_host()?;
            run_script("examples/saga/durability.sh")
        }
        // A saga whose legs are real durable Golem workers. Needs the Golem
        // binary — `cargo xtask e2e golem` fetches it once.
        "saga-golem" => {
            build_components(false)?;
            compose_app(Some("saga"), false)?;
            comp_host()?;
            run_script("examples/saga/golem-legs.sh")
        }
        // gate as a real Golem agent: exact serialization under a burst.
        "gate-golem" => run_script("examples/gate/golem-run.sh"),
        // The golem-workflow provider's live e2e (docs/capabilities/GOLEM.md
        // rung 3): downloads Golem 1.5, deploys the demo agent, invokes it.
        "golem" => run_script("providers/golem-workflow/e2e.sh"),
        // The `bytes:codec` spec run against the ARTIFACT, over HTTP through
        // codec-probe, so whatever satisfies the contract is what is judged.
        "gate-codec" => {
            comp_host()?;
            comp_plug()?;
            let mut cmd = Command::new("cargo");
            cmd.args(["build", "--release", "--target", "wasm32-wasip2", "-p", "bytes-codec", "-p", "codec-probe"])
                .current_dir("components");
            run_cmd(&mut cmd, "build bytes-codec + codec-probe")?;
            run_script("components/bytes-codec/gate.sh")
        }
        "binder-poly" => binder_poly(args),
        app => e2e_app(app, args),
    }
}

fn e2e_app(app: &str, args: &[String]) -> Result<()> {
    let dir = PathBuf::from(format!("examples/{app}"));
    let cargo_suite = dir.join("Cargo.toml").is_file();
    let browser_suite = dir.join("playwright.config.ts").is_file();
    if !cargo_suite && !browser_suite {
        anyhow::bail!(
            "{} has no e2e suite (no Cargo.toml, no playwright.config.ts). Scripted variants: \
             conformance-conduit, durable-saga, saga-golem, gate-golem, golem, gate-codec, binder-poly",
            dir.display()
        );
    }

    build_components(false)?;
    compose_app(Some(app), false)?;
    comp_host()?;

    if cargo_suite {
        // mesh's suite spawns its own flaky upstream from `target/release/flaky`,
        // and the old recipe built it first — any example with a binary gets the
        // same, so the test never finds it missing.
        if dir.join("src/bin").is_dir() || dir.join("src/main.rs").is_file() {
            let mut bins = Command::new("cargo");
            bins.args(["build", "--release", "--bins"]).current_dir(&dir);
            run_cmd(&mut bins, &format!("cargo build --release --bins ({app})"))?;
        }
        let mut test = Command::new("cargo");
        test.args(["test", "--release"]).args(args).current_dir(&dir);
        return run_cmd(&mut test, &format!("cargo test --release ({app})"));
    }

    // A browser suite, because what is asserted needs one (poll: one vote per
    // browser is a cookie rule; console: the page, not the body, shows the run).
    // Fails loudly when a prerequisite is missing rather than skipping.
    if app == "console" {
        let mut seed = Command::new("cargo");
        seed.args(["build", "--manifest-path", "reconciler/Cargo.toml", "--release", "--bin", "comp-trace-seed"]);
        run_cmd(&mut seed, "build comp-trace-seed")?;
    }
    let mut ci = Command::new("npm");
    ci.arg("ci").current_dir(&dir);
    run_cmd(&mut ci, &format!("npm ci ({app})"))?;
    let mut install = Command::new("npx");
    install.args(["playwright", "install", "--with-deps", "chromium"]).current_dir(&dir);
    run_cmd(&mut install, "playwright install chromium")?;
    let mut test = Command::new("npx");
    test.args(["playwright", "test"]).args(args).current_dir(&dir);
    run_cmd(&mut test, &format!("playwright test ({app})"))
}

/// The binder's own e2e, run against a composition built in another LANGUAGE.
///
/// `portfolio:value` and `price:history` are re-derived under
/// `components/<capability>-<lang>`, built through tools/build-polyglot.sh, and
/// swapped in for the Rust build by filename. Nothing in
/// examples/binder/tests/binder.rs is edited: the same assertions judge
/// whichever artifact satisfies the contract, which is the claim.
///
/// The Rust build is put back afterwards even when the test fails — it is what
/// `cargo xtask build` produces and what everything else composes against.
fn binder_poly(args: &[String]) -> Result<()> {
    let [lang, cap] = args else {
        anyhow::bail!("usage: cargo xtask e2e binder-poly <lang> <capability>  (e.g. go portfolio-value)");
    };
    build_components(false)?;
    let mut poly = Command::new("./tools/build-polyglot.sh");
    poly.args([lang, cap]);
    run_cmd(&mut poly, &format!("build-polyglot {lang} {cap}"))?;

    let plug = comp_plug()?;
    let binder = resolve_app("binder");
    let snake = cap.replace('-', "_");
    let rust_build = format!("{RELEASE}/{snake}.wasm");
    let backup = format!("components/target/{snake}.rust.wasm");
    fs::copy(&rust_build, &backup)?;

    let result = (|| -> Result<()> {
        fs::copy(format!("components/target/{snake}_{lang}.wasm"), &rust_build)?;
        derive(&plug, &binder.root, &binder.artifact)?;
        comp_host()?;
        let mut test = Command::new("cargo");
        test.args(["test", "--release"]).current_dir("examples/binder");
        run_cmd(&mut test, &format!("cargo test --release (binder, {cap} in {lang})"))
    })();

    fs::copy(&backup, &rust_build)?;
    derive(&plug, &binder.root, &binder.artifact)?;
    println!("restored the Rust build of {cap}");
    result
}

fn list_apps() -> Result<()> {
    println!("{}", "Registered Holon Applications (apps/*.toml):".cyan().bold());
    println!("{:<20} {:<8} {:<10} {:<30}", "APP", "PORT", "KV", "DOMAIN");
    println!("{:-<20} {:-<8} {:-<10} {:-<30}", "", "", "", "");

    let apps = comp_metadata::app::registered_apps(Path::new("."));
    for a in apps {
        let port_str = a.port.map_or("-".to_string(), |p| p.to_string());
        let kv_str = a.kv_as_string().unwrap_or_else(|| "-".to_string());
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
            compose_app(app.as_deref(), true)?;
        }

        Commands::Host { app, addr, kv } => {
            host_app(&app, addr.as_deref(), kv.as_deref())?;
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

        Commands::ContractCritic { goals } => {
            contract_critic(&goals)?;
        }

        Commands::WadmStatus => {
            wadm_status()?;
        }
    }

    Ok(())
}

// ---- contract-critic: was tools/contract-critic.py -------------------------
//
// The cheap, deterministic half of what a branch used to do by hand: read a
// goal's contract, find a claim the components do not support, before a
// generation was spent on it. Two shapes, no model call:
//   - a context file that does not resolve;
//   - a capability a part's world imports whose signature the contract never
//     quotes, so the part has to guess it.
// Ported from Python because it's a dev-tooling script with no component tie
// — this repo's own convention keeps those in Rust alongside xtask.

/// package namespace -> the binding alias a contract quotes it under
/// (e.g. `money:amount` is quoted as `money::`, `idempotency:*` as `idem::`).
fn contract_alias(ns: &str) -> &str {
    match ns {
        "idempotency" => "idem",
        "ratelimit" => "rl",
        "quota" => "meter",
        "otp" => "totp",
        "session" => "sessions",
        "event" => "bus",
        "auth" => "authz",
        "lock" => "mutex",
        other => other,
    }
}

/// The namespaces a `.wit` file's world imports (`import wasi:http/...;` -> `wasi`).
fn wit_imports(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("import "))
        .filter_map(|rest| rest.split(':').next())
        .map(str::to_string)
        .collect()
}

fn contract_critic_one(goal_path: &Path) -> Result<bool> {
    let goal: toml::Value = toml::from_str(
        &fs::read_to_string(goal_path).with_context(|| format!("reading {}", goal_path.display()))?,
    )?;
    let title = goal
        .get("title")
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| goal_path.display().to_string());

    let Some(parts) = goal.get("part").and_then(|p| p.as_array()) else {
        println!("{}: single-part goal, nothing to cross-check", goal_path.display());
        return Ok(true);
    };
    let contract_path = goal
        .get("contract")
        .and_then(|c| c.as_str())
        .with_context(|| format!("{}: no `contract` key", goal_path.display()))?;
    let contract = fs::read_to_string(contract_path)
        .with_context(|| format!("reading contract {contract_path}"))?;

    let mut problems: BTreeSet<String> = BTreeSet::new();
    for part in parts {
        let name = part.get("name").and_then(|n| n.as_str()).unwrap_or("?");
        let context: Vec<&str> = part
            .get("context")
            .and_then(|c| c.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        // 1. Every context path must resolve.
        for c in &context {
            if !Path::new(c).exists() {
                problems.insert(format!("{name}: context path does not exist: {c}"));
            }
        }

        // 2. Every capability a part's world imports should have its signature
        //    quoted in the contract — the world is the app's own .wit, among
        //    the part's context.
        for w in context.iter().filter(|c| c.ends_with(".wit") && c.contains("-domain/")) {
            let Ok(text) = fs::read_to_string(w) else { continue };
            for ns in wit_imports(&text) {
                if ns == "wasi" || ns == "comp" {
                    continue;
                }
                let alias = contract_alias(&ns);
                let covered =
                    contract.contains(&format!("{alias}::")) || contract.contains(&format!("{ns}:"));
                if !covered {
                    problems.insert(format!(
                        "{name}: world imports `{ns}:` but the contract never quotes \
                         `{alias}::` — the part must guess the signature"
                    ));
                }
            }
        }
    }

    if problems.is_empty() {
        println!("OK  {title}");
        Ok(true)
    } else {
        println!("FAIL {title}");
        for p in &problems {
            println!("  · {p}");
        }
        Ok(false)
    }
}

fn contract_critic(goals: &[PathBuf]) -> Result<()> {
    if goals.is_empty() {
        anyhow::bail!("usage: cargo xtask contract-critic <goal.toml> [<goal.toml> ...]");
    }
    let mut all_ok = true;
    for g in goals {
        if !contract_critic_one(g)? {
            all_ok = false;
        }
    }
    if all_ok {
        Ok(())
    } else {
        anyhow::bail!("one or more goals have contract problems");
    }
}

// ---- wadm-status: was tools/wadm-status.py ---------------------------------
//
// wadm's own status JSON is nested and verbose enough that reading it raw is
// how a real failure gets missed. Reads `wadm.api.<lattice>.model.status.<app>`
// on stdin, prints one line per scaler and the reason when it failed.

fn wadm_status() -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).context("reading stdin")?;
    let v: serde_json::Value = serde_json::from_str(&input).context("parsing wadm status JSON")?;
    let st = v.get("status").unwrap_or(&v);
    let scalers = st.get("scalers").and_then(|s| s.as_array()).cloned().unwrap_or_default();

    if scalers.is_empty() {
        // wadm ignores a trait type it does not understand rather than
        // refusing it, so "no scalers" is what a v2 manifest on a v1 wadm
        // looks like: deployed, running nothing. Worth saying out loud.
        println!("  no scalers — if this manifest was rendered --api v2, this wadm ignored it");
    }
    for s in &scalers {
        let kind = s.get("kind").and_then(|k| k.as_str()).unwrap_or("?");
        let name: String = s.get("name").and_then(|n| n.as_str()).unwrap_or("?").chars().take(46).collect();
        let status_type = s.pointer("/status/type").and_then(|t| t.as_str()).unwrap_or("?");
        println!("  {kind:<12} {name:<46} {status_type}");
        if status_type == "failed" {
            let msg: String =
                s.pointer("/status/message").and_then(|m| m.as_str()).unwrap_or("").chars().take(160).collect();
            println!("               {msg}");
        }
    }
    Ok(())
}
