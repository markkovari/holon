//! `cargo xtask compose` and `cargo xtask list` — turning an app name into a
//! composed artifact via `comp-plug`, and listing what `apps/*.toml` register.

use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::util::{run_cmd, RELEASE};

/// What `compose` hands comp-plug for an app, and where the result lands.
pub(crate) struct ResolvedApp {
    /// The component comp-plug composes from its own imports.
    pub(crate) root: String,
    /// `components/target/<file>` — the path `host` serves and the e2e tests read.
    pub(crate) artifact: String,
}

/// One answer to "which component is app X, and where does its composition go",
/// shared by `compose`, `host` and `e2e` — they each used to guess separately,
/// and disagreed: `compose` looked for a `<name>-domain` crate while `host`
/// looked for `<name>_domain.composed.wasm`, so `authgate` (root `mfa-authgate`)
/// failed to compose and `eshop` composed to a file `host` never looked for.
pub(crate) fn resolve_app(name: &str) -> ResolvedApp {
    // apps/<name>.toml first: its `artifact` (and `root`, if it sets one) are the
    // app's own statement of what it is. `discover_apps` resolves the root from
    // them the same way `holon` does, including the `-domain`-first rule below.
    if let Some(a) =
        comp_metadata::app::discover_apps(Path::new(".")).into_iter().find(|a| a.name == name)
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
pub(crate) fn derive(comp_plug_bin: &Path, root: &str, dest: &str) -> Result<()> {
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
pub(crate) fn comp_plug() -> Result<PathBuf> {
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
pub(crate) fn comp_host() -> Result<()> {
    let mut build_host = Command::new("cargo");
    build_host.args([
        "build",
        "--manifest-path",
        "host/Cargo.toml",
        "--release",
        "--bin",
        "comp-host",
    ]);
    run_cmd(&mut build_host, "build comp-host")
}

pub(crate) fn compose_app(app: Option<&str>, build_ui: bool) -> Result<()> {
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
                    if p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("grocery-domain."))
                    {
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
                anyhow::bail!(
                    "comp-plug intent-router failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
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

pub(crate) fn list_apps() -> Result<()> {
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
