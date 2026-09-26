//! `cargo xtask host` — run an app on `comp-host`, starting whatever native
//! daemons it declares first (ADR-0095's twelve capabilities are otherwise
//! silently unusable).

use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::compose::{comp_host, compose_app, resolve_app};
use crate::util::{run_cmd, RELEASE};

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
pub(crate) fn seed_studio(addr: &str) -> Result<()> {
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

pub(crate) fn host_app(
    app: &str,
    addr: Option<&str>,
    kv: Option<&str>,
    extra_config: &[String],
) -> Result<()> {
    for kv in extra_config {
        if !kv.contains('=') {
            anyhow::bail!("--config {kv}: expected KEY=VALUE");
        }
    }
    // The same path `compose` writes, so `compose X && host X` finds it.
    let artifact_path = resolve_app(app).artifact;

    // Check spec for port/kv/static_dir
    let specs = comp_metadata::app::registered_apps(Path::new("."));
    let app_spec = specs.iter().find(|s| s.name == app);
    let default_port = app_spec.and_then(|s| s.port).unwrap_or(3055);
    let default_kv =
        app_spec.and_then(|s| s.kv_as_string()).unwrap_or_else(|| "sqlite".to_string());

    if !Path::new(&artifact_path).exists() {
        println!(
            "{}",
            format!("Artifact {artifact_path} not found. Composing {app} first...").yellow()
        );
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

    let bind_addr =
        addr.map(|a| a.to_string()).unwrap_or_else(|| format!("0.0.0.0:{default_port}"));
    let kv_mode = kv.map(|k| k.to_string()).unwrap_or(default_kv);

    println!(
        "{}",
        format!("Starting {app} on http://{bind_addr} (kv: {kv_mode})...").green().bold()
    );

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

    // The app's whole `[config]` (daemon `<name>-url`/`-token` included) and
    // the egress allow-list to reach its daemons — comp-host denies all
    // outbound HTTP by default. Passing only the daemon keys left every other
    // key unset: photoquest's `public-callback-base` was, and `complete`
    // answered 503.
    if let Some(spec) = app_spec {
        host_cmd.args(spec.host_args());
    }
    // After the spec's: comp-host applies `--config` in order, the last wins.
    for kv in extra_config {
        host_cmd.args(["--config", kv]);
    }

    // The store the spec (or `--kv`) asked for. This used to be printed above and
    // never passed, so comp-host fell back to `memory` and every account was gone
    // on the next restart while the banner said "kv: sqlite".
    host_cmd.args(["--kv", &kv_mode]);
    // A local sqlite file belongs under target/, not in the repo root where
    // comp-host's own fallback (./comp-kv.db) would put it. `STATE_DIRECTORY`,
    // when the caller set one (systemd, or e2e/photoquest.sh's temp dir), wins —
    // comp-host already reads it.
    if kv_mode == "sqlite" && std::env::var_os("STATE_DIRECTORY").is_none_or(|v| v.is_empty()) {
        let dir = Path::new("target/state").join(app);
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let db = dir.join("kv.db");
        println!("{}", format!("  state: {}", db.display()).dimmed());
        host_cmd.arg("--sqlite-path").arg(&db);
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
