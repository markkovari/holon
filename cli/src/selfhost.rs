use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use clap::ValueEnum;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Spec {
    pub name: String,
    pub domain: String,
    /// Relative to the repo root; `cargo xtask compose <app>` produces it.
    pub artifact: String,

    /// Derived from the name when absent; `validate` refuses duplicates either way.
    #[serde(default)]
    pub port: Option<u16>,
    /// The `--router tailscale-serve` mount, `/<name>` by default since apps share one hostname.
    /// An SPA needs `mount = "/"`: its absolute `/api` and `/assets` paths miss under a prefix.
    #[serde(default)]
    pub mount: Option<String>,
    #[serde(default = "default_kv")]
    pub kv: String,
    #[serde(default)]
    pub kv_url: Option<String>,
    #[serde(default)]
    pub static_dir: Option<String>,
    /// `false` emits `--no-pool`; pooling measured 3.1x faster (ADR-0057).
    #[serde(default = "default_true")]
    pub pooling: bool,

    /// `tailnet` (default, fail closed: Tailscale address, Caddy's local CA) or `public` (ACME).
    #[serde(default = "default_access")]
    pub access: String,

    #[serde(default)]
    pub config: BTreeMap<String, String>,

    /// For tier 3 (Kubernetes); present so moving a spec up a tier is not a rewrite.
    #[serde(default)]
    pub components: Vec<String>,
    // Read by tier 3 and round-tripped by this file's test, never by this renderer.
    #[serde(default)]
    #[allow(dead_code)]
    pub strategy: Option<String>,

    /// `sched:timer`, `event:bus` and `cron:expr` are pull: without a relay, timers never fire.
    #[serde(default)]
    pub triggers: Option<Triggers>,

    /// Native daemons (ADR-0095) the component dials over loopback; absent, it answers `unavailable`.
    #[serde(default, rename = "daemon")]
    pub daemons: Vec<Daemon>,
}

/// Only what a deployment decides; daemon defaults live in the daemon's package (ADR-0115).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Daemon {
    /// `comp-<name>` is the binary; `check()` makes `addr` agree with `[config]`'s `<name>-url`.
    pub name: String,
    /// Absent makes it a worker that listens on nothing (`comp-resume`, ADR-0106): no `--addr`,
    /// no `<name>-url`, no token, no egress from the app.
    #[serde(default)]
    pub addr: Option<String>,
    /// Named, not inferred: only the caller knows which allow-list flag its daemon takes.
    #[serde(default)]
    pub allow_flag: Option<String>,
    #[serde(default)]
    pub allow: Vec<String>,
    /// Passed verbatim, already `--flag value` shaped.
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Loopback alone is not a boundary; must agree with `[config]`'s `<name>-token`.
    #[serde(default)]
    pub token: Option<String>,
}

/// `-daemon` suffix when named like its app, or it would overwrite the app's `comp-<app>.service`.
pub fn daemon_unit_name(spec: &Spec, d: &Daemon) -> String {
    if d.name == spec.name {
        format!("comp-{}-daemon.service", d.name)
    } else {
        format!("comp-{}.service", d.name)
    }
}

pub fn unit_names(spec: &Spec) -> Vec<String> {
    let mut v = vec![format!("comp-{}.service", spec.name)];
    if spec.triggers.is_some() {
        v.push(format!("comp-{}-relay.service", spec.name));
    }
    v.extend(spec.daemons.iter().map(|d| daemon_unit_name(spec, d)));
    v
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Triggers {
    #[serde(default = "default_pump")]
    pub pump: String,
    /// Short enough that a missed push is a delay, not a stall.
    #[serde(default = "default_sweep")]
    pub interval: u64,
}

fn default_pump() -> String {
    "/internal/pump".into()
}
fn default_sweep() -> u64 {
    10
}

fn default_kv() -> String {
    // Durable default: `Restart=always` makes restarts routine; comp-host uses $STATE_DIRECTORY.
    "sqlite".into()
}
fn default_access() -> String {
    // Fail closed: private unless the spec says otherwise.
    "tailnet".into()
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Router {
    Caddy,
    Traefik,
    /// No proxy at all: `tailscale serve` fronts the app, and Tailscale mints a
    /// browser-trusted certificate for the node's own name. One hostname per
    /// machine, so several apps are distinguished by path.
    TailscaleServe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GoaldFormat {
    /// A hardened unit for tier 1's own box: `docs/SELFHOST.md`'s
    /// `comp-host` + systemd + Caddy.
    Systemd,
    /// A `launchd` LaunchAgent for a developer's own Mac — no systemd there.
    Launchd,
}

pub struct Layout {
    pub bin: PathBuf,
    pub app_dir: PathBuf,
    pub env_file: PathBuf,
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            bin: PathBuf::from("/usr/local/bin/comp-host"),
            app_dir: PathBuf::from("/srv/comp"),
            env_file: PathBuf::from("/etc/comp"),
        }
    }
}

fn is_dns_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.starts_with(|c: char| c.is_ascii_alphanumeric())
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Deterministic so a re-render never moves a running app; `validate` catches collisions.
pub fn derived_port(name: &str) -> u16 {
    let mut h: u32 = 2166136261;
    for b in name.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(16777619);
    }
    30000 + (h % 1000) as u16
}

pub fn port_of(spec: &Spec) -> u16 {
    spec.port.unwrap_or_else(|| derived_port(&spec.name))
}

pub fn check(spec: &Spec) -> Result<()> {
    if !is_dns_label(&spec.name) {
        bail!("name {:?} must be a lowercase DNS label", spec.name);
    }
    if spec.domain.trim().is_empty() || spec.domain.contains(char::is_whitespace) {
        bail!("domain {:?} is not a hostname", spec.domain);
    }
    if !matches!(spec.access.as_str(), "tailnet" | "public") {
        bail!("access must be tailnet|public, got {:?}", spec.access);
    }
    if spec.access == "public" && spec.domain.ends_with(".ts.net") {
        bail!(
            "domain {:?} is a Tailscale name, which cannot be reached or certified publicly — use access = \"tailnet\"",
            spec.domain
        );
    }
    if !matches!(spec.kv.as_str(), "memory" | "sqlite" | "redis" | "nats") {
        bail!("kv must be memory|sqlite|redis|nats, got {:?}", spec.kv);
    }
    if matches!(spec.kv.as_str(), "redis" | "nats") && spec.kv_url.is_none() {
        bail!("kv = {:?} needs kv_url", spec.kv);
    }
    for k in spec.config.keys() {
        if k.is_empty()
            || !k.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            bail!("config key {k:?} must be lowercase-with-dashes");
        }
    }
    for d in &spec.daemons {
        // Interpolated into `ExecStart=`: `%` is a systemd specifier and a newline injects a directive.
        let clean = |v: &str| !v.contains('%') && !v.contains('\n');
        if !clean(&d.name)
            || !d.allow.iter().all(|a| clean(a))
            || !d.extra_args.iter().all(|a| clean(a))
        {
            bail!("daemon {:?}: name/allow/extra_args may not contain '%' or a newline", d.name);
        }
        let Some(addr) = &d.addr else {
            // A worker listens on nothing, so a token or a `<name>-url` would mean something the unit can't do.
            if d.token.is_some() {
                bail!(
                    "daemon {:?} has no addr, so nothing connects to it and there is no token \
                     for it to require — give it the credentials it uses itself in extra_args",
                    d.name
                );
            }
            let key = format!("{}-url", d.name);
            if spec.config.contains_key(&key) {
                bail!(
                    "[config] {key} points a component at daemon {:?}, which has no addr and \
                     listens on nothing",
                    d.name
                );
            }
            continue;
        };
        if !addr.contains(':') {
            bail!("daemon {:?} addr {:?} is not host:port", d.name, addr);
        }
        // The unit binds `addr`, the component reads `<name>-url`: nothing else makes them agree.
        let key = format!("{}-url", d.name);
        match spec.config.get(&key) {
            Some(v) if *v == format!("http://{addr}") => {}
            Some(v) => bail!(
                "daemon {:?} binds {:?} but [config] {key} = {v:?} — they must agree",
                d.name,
                addr
            ),
            None => bail!("daemon {:?} has no matching [config] {key}", d.name),
        }
        // Same for `token` vs `<name>-token`; absent on both sides means no auth.
        if let Some(want) = &d.token {
            let key = format!("{}-token", d.name);
            match spec.config.get(&key) {
                Some(v) if v == want => {}
                Some(v) => bail!(
                    "daemon {:?} token {:?} but [config] {key} = {v:?} — they must agree",
                    d.name,
                    want
                ),
                None => {
                    bail!("daemon {:?} sets a token but has no matching [config] {key}", d.name)
                }
            }
        }
    }
    // Two units under one file name: the last write silently wins.
    let mut seen = BTreeSet::new();
    for u in unit_names(spec) {
        if !seen.insert(u.clone()) {
            bail!(
                "two units of {:?} would both be {u}, and one would be written over the other",
                spec.name
            );
        }
    }
    Ok(())
}

pub fn write_secret_file(path: &Path, content: &[u8]) -> Result<()> {
    std::fs::write(path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn render_env(spec: &Spec) -> String {
    let mut s = String::new();
    s.push_str("# Generated by selfhost — do not edit; edit the app spec and re-deploy.\n");
    s.push_str("# `wasi:config` for this app, read by `comp-host --config-file`.\n");
    for (k, v) in &spec.config {
        // One line per key, so a newline in a value would forge a second key.
        let clean = v.replace('\n', " ");
        s.push_str(&format!("{k} = {clean}\n"));
    }
    s
}

pub fn render_unit(spec: &Spec, layout: &Layout) -> String {
    let port = port_of(spec);
    let app = &spec.name;
    let mut args = format!(
        "--component {}/{}/app.wasm --addr 127.0.0.1:{} --kv {}",
        layout.app_dir.display(),
        app,
        port,
        spec.kv
    );
    if let Some(url) = &spec.kv_url {
        let flag = if spec.kv == "redis" { "--redis-url" } else { "--nats-url" };
        args.push_str(&format!(" {flag} {url}"));
    }
    if spec.static_dir.is_some() {
        args.push_str(&format!(" --static-dir {}/{}/static", layout.app_dir.display(), app));
    }
    // Pooling is the default, so only `--no-pool` is emitted (ADR-0057).
    if !spec.pooling {
        args.push_str(" --no-pool");
    }
    // comp-host denies egress and loopback by default; the allow-list is only daemon IP literals,
    // so `--allow-private-egress` opens nothing a name could resolve into.
    let reachable: Vec<&str> = spec.daemons.iter().filter_map(|d| d.addr.as_deref()).collect();
    for addr in &reachable {
        args.push_str(&format!(" --egress {addr}"));
    }
    if !reachable.is_empty() {
        args.push_str(" --allow-private-egress");
    }

    let mut s = String::new();
    s.push_str("# Generated by selfhost — do not edit; edit the app spec and re-deploy.\n");
    s.push_str(&format!("[Unit]\nDescription=comp-host: {app} ({})\n", spec.domain));
    s.push_str("After=network-online.target\nWants=network-online.target\n\n");
    s.push_str("[Service]\nType=simple\n");
    // Via a systemd credential: the 0600 root-owned file is unreadable under DynamicUser, and
    // `--config k=v` would put every value in `ps`. `%d` is the credential directory.
    if !spec.config.is_empty() {
        args.push_str(" --config-file %d/config");
    }
    if !spec.config.is_empty() {
        s.push_str(&format!("LoadCredential=config:{}/{}.env\n", layout.env_file.display(), app));
    }
    s.push_str(&format!("ExecStart={} {}\n", layout.bin.display(), args));
    s.push_str("Restart=always\nRestartSec=2\n");
    s.push_str("DynamicUser=yes\n");
    s.push_str(&format!("StateDirectory=comp/{app}\n"));
    s.push_str("NoNewPrivileges=yes\n");
    s.push_str("PrivateTmp=yes\n");
    s.push_str("PrivateDevices=yes\n");
    s.push_str("ProtectSystem=strict\n");
    s.push_str("ProtectHome=yes\n");
    s.push_str("ProtectKernelTunables=yes\n");
    s.push_str("ProtectControlGroups=yes\n");
    s.push_str("RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX\n");
    s.push_str("RestrictNamespaces=yes\n");
    s.push_str("LockPersonality=yes\n");
    // Own line: systemd has no inline comments, so `key=value # why` would join the value.
    s.push_str("# wasmtime JITs, so it needs W^X — this one cannot be tightened.\n");
    s.push_str("MemoryDenyWriteExecute=no\n");
    s.push_str("\n[Install]\nWantedBy=multi-user.target\n");
    s
}

/// One site for the whole lattice: the ingress routes by `Host`, so this never needs regenerating.
pub fn render_ingress_route(domain: &str, upstream: &str, tailnet: bool) -> String {
    let head = "# Generated by `comp node ingress` — do not edit.\n\
                #\n\
                # Fronts comp-ingress, which routes by Host header from lattice\n\
                # inventory. Deploying an app does NOT require regenerating this:\n\
                # the ingress learns the route from the node that runs it.\n";
    if tailnet {
        format!(
            "{head}#\n\
             # TAILNET ONLY. `bind {{$TS_IP}}` listens on the Tailscale address alone, so\n\
             # this is unreachable from any public interface; `tls internal` uses Caddy's\n\
             # own CA, so there is no ACME, no public DNS record, and still a secure\n\
             # context. Trust Caddy's root once per device.\n\
             {domain} {{\n\tbind {{$TS_IP}}\n\ttls internal\n\treverse_proxy {upstream}\n}}\n"
        )
    } else {
        format!(
            "{head}# PUBLIC: certificate over HTTP-01. :80 and :443 must be open.\n\
             {domain} {{\n\treverse_proxy {upstream}\n}}\n"
        )
    }
}

/// All but `MemoryDenyWriteExecute`, which each caller adds since comp-host JITs and needs it off.
fn hardening(restart_sec: u64) -> String {
    format!(
        "Restart=always\nRestartSec={restart_sec}\nDynamicUser=yes\nNoNewPrivileges=yes\n\
         PrivateTmp=yes\nPrivateDevices=yes\nProtectSystem=strict\nProtectHome=yes\n\
         ProtectKernelTunables=yes\nProtectControlGroups=yes\n\
         RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX\nRestrictNamespaces=yes\n\
         LockPersonality=yes\n"
    )
}

/// A separate unit dialling loopback, not the public hostname: going through Caddy would
/// expose an internal endpoint wherever the route is reachable.
pub fn render_relay_unit(spec: &Spec, t: &Triggers, l: &Layout) -> String {
    let mut s =
        String::from("# Generated by selfhost — do not edit; edit the app spec and re-deploy.\n");
    s.push_str(&format!(
        "[Unit]\nDescription=comp-relay: {} timers and topics\n\
         # The app is what it pokes, so it is pointless before that starts and should\n\
         # go down with it.\n\
         After={unit}\nBindsTo={unit}\n\n[Service]\nType=simple\n",
        spec.name,
        unit = format!("comp-{}.service", spec.name)
    ));
    s.push_str(&format!(
        "ExecStart={}/comp-relay --target http://127.0.0.1:{}{} --interval {}\n",
        l.bin.parent().map(|p| p.display().to_string()).unwrap_or_else(|| "/usr/local/bin".into()),
        port_of(spec),
        t.pump,
        t.interval
    ));
    // No lease: tier 1 runs one copy of one app, so there is nothing to elect between.
    s.push_str(&hardening(2));
    // Unlike comp-host, this one JITs nothing — it is an HTTP client with a clock.
    s.push_str("MemoryDenyWriteExecute=yes\n");
    s.push_str("\n[Install]\nWantedBy=multi-user.target\n");
    s
}

/// ADR-0118: daemons migrated to `holon-` use the new prefix in their service unit.
pub fn daemon_bin_name(name: &str) -> String {
    match name {
        "ffmpeg" | "docker" | "imageopt" | "llmlocal" | "mdns" | "browser" | "clipboard"
        | "cron" | "lanscan" | "uinotify" | "wireguard" | "media" => {
            format!("holon-{name}")
        }
        _ => format!("comp-{name}"),
    }
}

/// One unit per daemon so they don't share a blast radius, bound to the app like the relay.
pub fn render_daemon_unit(spec: &Spec, d: &Daemon, l: &Layout) -> String {
    let bin_dir =
        l.bin.parent().map(|p| p.display().to_string()).unwrap_or_else(|| "/usr/local/bin".into());
    let bin = daemon_bin_name(&d.name);
    let mut parts: Vec<String> = Vec::new();
    if let Some(addr) = &d.addr {
        parts.push(format!("--addr {addr}"));
    }
    if let Some(flag) = &d.allow_flag {
        for v in &d.allow {
            parts.push(format!("--{flag} {v}"));
        }
    }
    parts.extend(d.extra_args.iter().cloned());
    // `--token-file`, not `--token`: argv is `ps`-readable by any local user.
    if d.token.is_some() {
        parts.push("--token-file %d/token".into());
    }
    let args = parts.join(" ");
    let what = match &d.addr {
        Some(_) => format!("native daemon behind {} (ADR-0095)", spec.name),
        None => format!("native worker beside {}, listening on nothing", spec.name),
    };

    let mut s =
        String::from("# Generated by selfhost — do not edit; edit the app spec and re-deploy.\n");
    s.push_str(&format!(
        "[Unit]\nDescription={bin}: {what}\n\
         After={unit}\nBindsTo={unit}\n\n[Service]\nType=simple\n",
        unit = format!("comp-{}.service", spec.name)
    ));
    if d.token.is_some() {
        s.push_str(&format!(
            "LoadCredential=token:{}/{}-{}.token\n",
            l.env_file.display(),
            spec.name,
            d.name
        ));
    }
    s.push_str(&format!("ExecStart={bin_dir}/{bin} {args}\n"));
    s.push_str(&hardening(2));
    // Plain Rust binaries, no JIT, so W^X stays on.
    s.push_str("MemoryDenyWriteExecute=yes\n");
    // `ProtectSystem=strict` would refuse the very paths the daemon's `--allow-path` grants.
    if d.allow_flag.as_deref() == Some("allow-path") && !d.allow.is_empty() {
        s.push_str(&format!("ReadWritePaths={}\n", d.allow.join(" ")));
    }
    s.push_str("\n[Install]\nWantedBy=multi-user.target\n");
    s
}

/// One `comp-goald` deployment draining one project's goal queue (ADR-0082, ADR-0096).
/// Its own spec: it is scoped to a repository, not to a deployed app.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoaldSpec {
    pub project: String,
    pub checkout: String,
    pub repo: String,
    #[serde(default)]
    pub platform_url: Option<String>,
    /// Re-login account for when the ~1h platform session expires; requires `password_file`.
    #[serde(default)]
    pub email: Option<String>,
    /// A path, never the value; read through `LoadCredential` like `Daemon::token`.
    #[serde(default)]
    pub password_file: Option<String>,
    #[serde(default = "default_max_runs")]
    pub max_runs: u32,
    #[serde(default = "default_poll")]
    pub poll: u64,
    /// Hold DeepSeek runs until its off-peak window (`comp_fuel::offpeak`), up to 2x cheaper.
    #[serde(default)]
    pub enforce_deepseek_offpeak: bool,
    /// `YYYY-MM-DD` off-peak-all-day lines; only with `enforce_deepseek_offpeak`.
    #[serde(default)]
    pub holidays: Option<String>,
    /// Passed verbatim to `comp-goalrun`.
    #[serde(default)]
    pub goalrun_args: Vec<String>,
}

fn default_max_runs() -> u32 {
    1
}
fn default_poll() -> u64 {
    15
}

pub fn check_goald(spec: &GoaldSpec) -> Result<()> {
    if spec.project.trim().is_empty() {
        bail!("project must not be empty");
    }
    if spec.checkout.trim().is_empty() {
        bail!("checkout must not be empty");
    }
    if !spec.repo.contains('/') {
        bail!("repo {:?} must be owner/name", spec.repo);
    }
    // Either alone silently becomes `login: None` in comp-goald, which dies on the first renewal.
    if spec.email.is_some() != spec.password_file.is_some() {
        bail!("email and password_file must be set together, or not at all");
    }
    // Interpolated into `ExecStart=`, same hazard as `render_daemon_unit`.
    let clean = |v: &str| !v.contains('%') && !v.contains('\n');
    let mut fields = vec![spec.project.as_str(), spec.checkout.as_str(), spec.repo.as_str()];
    fields.extend(spec.platform_url.as_deref());
    fields.extend(spec.email.as_deref());
    fields.extend(spec.holidays.as_deref());
    if !fields.iter().all(|v| clean(v)) || !spec.goalrun_args.iter().all(|a| clean(a)) {
        bail!("no field may contain '%' or a newline");
    }
    Ok(())
}

/// Not bound to any app unit: it is not a sidecar and outlives any single app.
pub fn render_goald_unit(spec: &GoaldSpec, l: &Layout) -> String {
    let bin_dir =
        l.bin.parent().map(|p| p.display().to_string()).unwrap_or_else(|| "/usr/local/bin".into());

    let mut args =
        format!("--project {} --checkout {} --repo {}", spec.project, spec.checkout, spec.repo);
    if let Some(url) = &spec.platform_url {
        args.push_str(&format!(" --platform-url {url}"));
    }
    if let Some(email) = &spec.email {
        args.push_str(&format!(" --email {email}"));
    }
    if spec.password_file.is_some() {
        // The systemd credential, not the real path, as for a daemon's token.
        args.push_str(" --password-file %d/password");
    }
    args.push_str(&format!(" --max-runs {} --poll {}", spec.max_runs, spec.poll));
    if spec.enforce_deepseek_offpeak {
        args.push_str(" --enforce-deepseek-offpeak");
    }
    if let Some(h) = &spec.holidays {
        args.push_str(&format!(" --holidays {h}"));
    }
    if !spec.goalrun_args.is_empty() {
        args.push_str(" -- ");
        args.push_str(&spec.goalrun_args.join(" "));
    }

    let mut s =
        String::from("# Generated by selfhost — do not edit; edit the goald spec and re-deploy.\n");
    s.push_str(&format!(
        "[Unit]\nDescription=comp-goald: drains {}'s goal queue (ADR-0082, ADR-0096)\n\
         After=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\n",
        spec.project
    ));
    if spec.password_file.is_some() {
        s.push_str(&format!(
            "LoadCredential=password:{}\n",
            spec.password_file.as_deref().unwrap()
        ));
    }
    s.push_str(&format!("ExecStart={bin_dir}/comp-goald {args}\n"));
    // Restart is tier 1's only recovery when a run outlives its ~1h session; 5s rather than 2s
    // so a tight loop doesn't burn CPU while that clock ticks.
    s.push_str(&hardening(5));
    // Shells out to plain binaries, no wasmtime inside, so W^X stays on.
    s.push_str("MemoryDenyWriteExecute=yes\n");
    // comp-goalrun commits into the checkout; `ProtectSystem=strict` would make it read-only.
    s.push_str(&format!("ReadWritePaths={}\n", spec.checkout));
    s.push_str("\n[Install]\nWantedBy=multi-user.target\n");
    s
}

/// `password_value`: the real path for launchd (runs as the owning uid), `%d/password` for systemd.
fn goald_argv(spec: &GoaldSpec, password_value: Option<&str>) -> Vec<String> {
    let mut a = vec![
        "--project".to_string(),
        spec.project.clone(),
        "--checkout".to_string(),
        spec.checkout.clone(),
        "--repo".to_string(),
        spec.repo.clone(),
    ];
    if let Some(url) = &spec.platform_url {
        a.push("--platform-url".into());
        a.push(url.clone());
    }
    if let Some(email) = &spec.email {
        a.push("--email".into());
        a.push(email.clone());
    }
    if let Some(pf) = password_value {
        a.push("--password-file".into());
        a.push(pf.to_string());
    }
    a.push("--max-runs".into());
    a.push(spec.max_runs.to_string());
    a.push("--poll".into());
    a.push(spec.poll.to_string());
    if spec.enforce_deepseek_offpeak {
        a.push("--enforce-deepseek-offpeak".into());
    }
    if let Some(h) = &spec.holidays {
        a.push("--holidays".into());
        a.push(h.clone());
    }
    if !spec.goalrun_args.is_empty() {
        a.push("--".into());
        a.extend(spec.goalrun_args.iter().cloned());
    }
    a
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// A macOS LaunchAgent: runs as the user's own uid, so no `DynamicUser`/`LoadCredential`.
pub fn render_goald_launchd(spec: &GoaldSpec, bin_dir: &Path, log_dir: &Path) -> String {
    let mut argv = vec![format!("{}/comp-goald", bin_dir.display())];
    argv.extend(goald_argv(spec, spec.password_file.as_deref()));
    let args_xml: String =
        argv.iter().map(|a| format!("\t\t<string>{}</string>\n", xml_escape(a))).collect();

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\
         \t<key>Label</key>\n\t<string>{label}</string>\n\
         \t<key>ProgramArguments</key>\n\t<array>\n{args_xml}\t</array>\n\
         \t<key>RunAtLoad</key>\n\t<true/>\n\
         \t<key>KeepAlive</key>\n\t<true/>\n\
         \t<key>StandardOutPath</key>\n\t<string>{out}</string>\n\
         \t<key>StandardErrorPath</key>\n\t<string>{err}</string>\n\
         </dict>\n</plist>\n",
        label = xml_escape(&format!("dev.holon.goald.{}", spec.project)),
        out = xml_escape(&log_dir.join(format!("comp-goald-{}.out.log", spec.project)).display().to_string()),
        err = xml_escape(&log_dir.join(format!("comp-goald-{}.err.log", spec.project)).display().to_string()),
    )
}

pub fn render_route(spec: &Spec, router: Router) -> String {
    let port = port_of(spec);
    let tailnet = spec.access == "tailnet";
    match router {
        Router::Caddy if tailnet => format!(
            "# Generated by selfhost — do not edit; edit the app spec and re-deploy.\n\
             #\n\
             # TAILNET ONLY. Two things make that true, and both matter:\n\
             #   bind {{$TS_IP}}  — listens on the Tailscale address alone, so this is\n\
             #                    unreachable from the VPS's public interface. The deploy\n\
             #                    recipe sets TS_IP from `tailscale ip -4` on the box.\n\
             #   tls internal   — a certificate from Caddy's own CA. No ACME, no DNS\n\
             #                    provider, no public record. Trust Caddy's root once per\n\
             #                    device (`caddy trust`, or install its root.crt) and the\n\
             #                    browser is happy — which also gives you a secure context,\n\
             #                    without which passkeys and service workers will not run.\n\
             #\n\
             # The hostname must resolve to that Tailscale address: a custom DNS record in\n\
             # the tailnet, or a split-DNS entry. MagicDNS alone gives one name per machine.\n\
             {} {{\n\tbind {{$TS_IP}}\n\ttls internal\n\treverse_proxy 127.0.0.1:{}\n}}\n",
            spec.domain, port
        ),
        Router::Caddy => format!(
            "# Generated by selfhost — do not edit; edit the app spec and re-deploy.\n\
             # PUBLIC: every interface, certificate over HTTP-01. :80 and :443 must be open.\n\
             {} {{\n\treverse_proxy 127.0.0.1:{}\n}}\n",
            spec.domain, port
        ),
        Router::TailscaleServe => format!(
            "#!/usr/bin/env bash\n\
             # Generated by selfhost — do not edit; edit the app spec and re-deploy.\n\
             #\n\
             # No proxy: Tailscale terminates TLS with a certificate it obtains for THIS\n\
             # NODE's name, so there is nothing to trust and nothing to renew.\n\
             #\n\
             # The limit is one hostname per machine — Tailscale certifies the node's own\n\
             # FQDN and not subdomains of it — so several apps share it by path. An app that\n\
             # assumes it is mounted at `/` will break here; that is the trade against the\n\
             # Caddy route, which gives a hostname per app.\n\
             set -euo pipefail\n\
             tailscale serve --bg --https=443 --set-path {mount} http://127.0.0.1:{port}\n\
             echo \"https://$(tailscale status --json | \\\n\
               python3 -c 'import json,sys;print(json.load(sys.stdin)[\"Self\"][\"DNSName\"].rstrip(\".\"))')\"\n",
            mount = spec.mount.clone().unwrap_or_else(|| format!("/{}", spec.name)),
            port = port
        ),
        Router::Traefik => format!(
            "# Generated by selfhost — do not edit; edit the app spec and re-deploy.\n\
             # Traefik file provider. Point `providers.file.directory` at this dir.\n\
             http:\n  routers:\n    {name}:\n      rule: \"Host(`{domain}`)\"\n      \
             service: {name}\n      entryPoints: [websecure]\n      tls:\n        \
             certResolver: le\n  services:\n    {name}:\n      loadBalancer:\n        \
             servers:\n          - url: \"http://127.0.0.1:{port}\"\n",
            name = spec.name,
            domain = spec.domain,
            port = port
        ),
    }
}

#[cfg(test)]
mod ingress_route_tests {
    use super::render_ingress_route;

    #[test]
    fn the_public_front_terminates_tls_and_forwards_plain() {
        let c = render_ingress_route("lattice.example.com", "127.0.0.1:8088", false);
        assert!(c.contains("lattice.example.com {"));
        assert!(c.contains("reverse_proxy 127.0.0.1:8088"));
        // No `tls` directive: Caddy's default is ACME.
        assert!(!c.contains("tls internal"));
    }

    #[test]
    fn the_tailnet_front_is_unreachable_publicly_and_still_a_secure_context() {
        // `bind` alone leaves it on http; `tls internal` alone listens everywhere.
        let c = render_ingress_route("lattice.ts.net", "127.0.0.1:8088", true);
        assert!(c.contains("bind {$TS_IP}"), "must not listen publicly");
        assert!(c.contains("tls internal"), "passkeys need a secure context");
    }

    #[test]
    fn one_site_fronts_the_whole_lattice_not_one_app() {
        let c = render_ingress_route("lattice.example.com", "127.0.0.1:8088", false);
        assert!(c.contains("does NOT require regenerating"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(toml_src: &str) -> Spec {
        let s: Spec = toml::from_str(toml_src).expect("parses");
        check(&s).expect("valid");
        s
    }

    const MINIMAL: &str = r#"
name = "gate"
domain = "gate.example.com"
artifact = "components/target/gate_domain.composed.wasm"
"#;

    #[test]
    fn a_minimal_spec_needs_three_lines() {
        let s = spec(MINIMAL);
        assert_eq!(s.kv, "sqlite");
        assert!(
            s.pooling,
            "pooling defaults on — it is what makes per-request instantiation cheap"
        );
        assert_eq!(port_of(&s), derived_port("gate"));
    }

    #[test]
    fn the_derived_port_is_stable_and_in_range() {
        for name in ["gate", "stash", "mesh", "a", "very-long-application-name"] {
            let p = derived_port(name);
            assert!((30000..31000).contains(&p), "{name} -> {p}");
            assert_eq!(p, derived_port(name), "not deterministic");
        }
    }

    #[test]
    fn the_unit_runs_comp_host_and_is_hardened() {
        let out = render_unit(&spec(MINIMAL), &Layout::default());
        assert!(out.contains("/usr/local/bin/comp-host"), "{out}");
        assert!(out.contains("--component /srv/comp/gate/app.wasm"));
        assert!(out.contains("--addr 127.0.0.1:"), "loopback only — the proxy is the front door");
        assert!(!out.contains("--addr 0.0.0.0"), "must never bind publicly: {out}");
        assert!(out.contains("Restart=always"));
        // No config, so no --config-file: a unit naming an unwritten file fails to start.
        assert!(!out.contains("--config-file"), "{out}");
        assert!(out.contains("DynamicUser=yes"));
        assert!(out.contains("ProtectSystem=strict"));
        assert!(out.contains("NoNewPrivileges=yes"));
        assert!(out.contains("StateDirectory=comp/gate"));
        assert!(out.contains("\nMemoryDenyWriteExecute=no\n"), "wasmtime JITs: {out}");

        for line in out.lines() {
            if line.starts_with('#') || line.starts_with('[') || line.trim().is_empty() {
                continue;
            }
            assert!(
                !line.contains(" #"),
                "inline comment would become part of the value: {line:?}"
            );
        }
    }

    #[test]
    fn config_reaches_the_host_through_the_start_command() {
        let with = spec(
            "name = \"gate\"\ndomain = \"g.example.com\"\nartifact = \"a.wasm\"\n\
             [config]\nk = \"v\"\n",
        );
        let unit = render_unit(&with, &Layout::default());
        assert!(unit.contains("LoadCredential=config:/etc/comp/gate.env"), "{unit}");
        assert!(unit.contains("--config-file %d/config"), "{unit}");
    }

    #[test]
    fn config_keys_are_written_the_way_the_host_reads_them() {
        let s = spec(
            r#"
name = "gate"
domain = "gate.example.com"
artifact = "a.wasm"
[config]
grace-period-secs = "5"
routes = "upstream=http://127.0.0.1:9000"
"#,
        );
        let out = render_env(&s);
        assert!(out.contains("grace-period-secs = 5"), "{out}");
        assert!(out.contains("routes = upstream=http://127.0.0.1:9000"), "{out}");
    }

    #[test]
    fn a_newline_in_a_value_cannot_forge_another_variable() {
        let s = spec(
            "name = \"gate\"\ndomain = \"g.example.com\"\nartifact = \"a.wasm\"\n\
             [config]\nk = \"one\\ntwo=three\"\n",
        );
        let out = render_env(&s);
        assert_eq!(out.lines().filter(|l| !l.starts_with('#')).count(), 1, "{out}");
        assert!(out.contains("k = one two=three"), "{out}");
    }

    #[test]
    fn each_app_gets_its_own_url() {
        let s = spec(MINIMAL);
        let caddy = render_route(&s, Router::Caddy);
        assert!(caddy.contains("gate.example.com {"), "{caddy}");
        assert!(caddy.contains(&format!("reverse_proxy 127.0.0.1:{}", port_of(&s))));

        let traefik = render_route(&s, Router::Traefik);
        assert!(traefik.contains("Host(`gate.example.com`)"), "{traefik}");
        assert!(traefik.contains(&format!("http://127.0.0.1:{}", port_of(&s))));
        assert!(traefik.contains("certResolver: le"), "TLS is the proxy's job: {traefik}");
    }

    #[test]
    fn private_is_the_default_so_a_forgotten_field_cannot_expose_an_app() {
        assert_eq!(spec(MINIMAL).access, "tailnet");
    }

    #[test]
    fn a_tailnet_app_binds_the_tailscale_address_and_uses_a_local_ca() {
        let out = render_route(&spec(MINIMAL), Router::Caddy);
        assert!(out.contains("bind {$TS_IP}"), "{out}");
        assert!(out.contains("tls internal"), "{out}");
        assert!(out.contains("reverse_proxy 127.0.0.1:"), "{out}");
    }

    #[test]
    fn a_public_app_does_not_bind_the_tailnet_and_uses_real_acme() {
        let s = spec(
            "name = \"blog\"\ndomain = \"blog.example.com\"\nartifact = \"a.wasm\"\n\
             access = \"public\"\n",
        );
        let out = render_route(&s, Router::Caddy);
        assert!(!out.contains("bind"), "public must listen everywhere: {out}");
        assert!(!out.contains("tls internal"), "public wants a real cert: {out}");
        assert!(out.contains("blog.example.com {"), "{out}");
    }

    #[test]
    fn a_ts_net_name_cannot_be_public() {
        let s: Spec = toml::from_str(
            "name = \"g\"\ndomain = \"box.tail1234.ts.net\"\nartifact = \"a.wasm\"\n\
             access = \"public\"\n",
        )
        .unwrap();
        let err = check(&s).unwrap_err().to_string();
        assert!(err.contains("Tailscale name"), "{err}");
        let ok: Spec = toml::from_str(
            "name = \"g\"\ndomain = \"box.tail1234.ts.net\"\nartifact = \"a.wasm\"\n",
        )
        .unwrap();
        assert!(check(&ok).is_ok());
    }

    #[test]
    fn tailscale_serve_needs_no_certificate_work_but_costs_the_hostname() {
        let out = render_route(&spec(MINIMAL), Router::TailscaleServe);
        assert!(out.contains("tailscale serve --bg --https=443"), "{out}");
        assert!(out.contains("--set-path /gate"), "{out}");
        assert!(out.contains(&format!("http://127.0.0.1:{}", port_of(&spec(MINIMAL)))));
        assert!(out.starts_with("#!/usr/bin/env bash"), "it is a script, not config");
    }

    #[test]
    fn an_app_that_owns_the_box_can_be_mounted_at_the_root() {
        let spec: Spec = toml::from_str(
            "name = \"events\"\ndomain = \"malna.tail3a9c.ts.net\"\n\
             artifact = \"a.wasm\"\nport = 3230\nmount = \"/\"\n",
        )
        .unwrap();
        let out = render_route(&spec, Router::TailscaleServe);
        assert!(out.contains("--set-path / http://127.0.0.1:3230"), "{out}");
        assert!(!out.contains("')/events"), "{out}");

        let shared: Spec = toml::from_str(
            "name = \"gate\"\ndomain = \"malna.tail3a9c.ts.net\"\nartifact = \"a.wasm\"\n",
        )
        .unwrap();
        assert!(render_route(&shared, Router::TailscaleServe).contains("--set-path /gate"));
    }

    #[test]
    fn sqlite_needs_no_url_and_no_path() {
        let s = spec(MINIMAL);
        let unit = render_unit(&s, &Layout::default());
        assert!(unit.contains("--kv sqlite"), "{unit}");
        assert!(!unit.contains("--sqlite-path"), "{unit}");
        assert!(unit.contains("StateDirectory=comp/gate"), "{unit}");
    }

    #[test]
    fn an_app_with_no_triggers_gains_no_poker() {
        assert!(spec(MINIMAL).triggers.is_none());
    }

    #[test]
    fn a_relay_unit_pokes_the_loopback_port_and_not_the_public_hostname() {
        let s = spec(&format!("{}\n[triggers]\n", MINIMAL));
        let t = s.triggers.as_ref().unwrap();
        assert_eq!(t.pump, "/internal/pump");
        let u = render_relay_unit(&s, t, &Layout::default());
        assert!(
            u.contains(&format!("--target http://127.0.0.1:{}/internal/pump", port_of(&s))),
            "{u}"
        );
        assert!(!u.contains(&s.domain), "the relay must not dial the public name: {u}");
        assert!(u.contains("BindsTo=comp-gate.service"), "{u}");
        assert!(u.contains("MemoryDenyWriteExecute=yes"), "{u}");
    }

    #[test]
    fn a_daemon_whose_addr_disagrees_with_its_config_is_refused() {
        let bad = "name = \"fs-watcher\"\ndomain = \"f.example.com\"\nartifact = \"a.wasm\"\n\
             [config]\nfswatch-url = \"http://127.0.0.1:9999\"\n\
             [[daemon]]\nname = \"fswatch\"\naddr = \"127.0.0.1:8000\"\n";
        let err = toml::from_str::<Spec>(bad).map(|s| check(&s)).expect("parses").unwrap_err();
        assert!(err.to_string().contains("must agree"), "{err}");
    }

    #[test]
    fn a_daemon_with_no_matching_config_key_is_refused() {
        let bad = "name = \"fs-watcher\"\ndomain = \"f.example.com\"\nartifact = \"a.wasm\"\n\
             [[daemon]]\nname = \"fswatch\"\naddr = \"127.0.0.1:8000\"\n";
        let err = toml::from_str::<Spec>(bad).map(|s| check(&s)).expect("parses").unwrap_err();
        assert!(err.to_string().contains("no matching"), "{err}");
    }

    #[test]
    fn an_app_unit_can_reach_its_daemons_and_nothing_else() {
        let s = spec(
            "name = \"fs-watcher\"\ndomain = \"f.example.com\"\nartifact = \"a.wasm\"\n\
             [config]\nfswatch-url = \"http://127.0.0.1:8000\"\n\
             [[daemon]]\nname = \"fswatch\"\naddr = \"127.0.0.1:8000\"\n",
        );
        let unit = render_unit(&s, &Layout::default());
        assert!(unit.contains("--egress 127.0.0.1:8000 --allow-private-egress"), "{unit}");
        assert_eq!(unit.matches("--egress").count(), 1, "{unit}");
        let none = render_unit(&spec(MINIMAL), &Layout::default());
        assert!(!none.contains("--egress") && !none.contains("--allow-private-egress"), "{none}");
    }

    const WITH_WORKER: &str = "name = \"park\"\ndomain = \"p.example.com\"\nartifact = \"a.wasm\"\n\
         [config]\npark-url = \"http://127.0.0.1:8015\"\n\
         [[daemon]]\nname = \"park\"\naddr = \"127.0.0.1:8015\"\n\
         [[daemon]]\nname = \"resume\"\n\
         extra_args = [\"--nats-url nats://127.0.0.1:4222\", \"--sink-url http://127.0.0.1:8016/x\"]\n";

    #[test]
    fn a_daemon_with_no_addr_is_a_worker_and_needs_nothing_a_listener_needs() {
        let s = spec(WITH_WORKER);
        assert_eq!(s.daemons[1].addr, None);
        assert_eq!(s.daemons[0].addr.as_deref(), Some("127.0.0.1:8015"));
    }

    #[test]
    fn a_workers_unit_has_no_addr_and_binds_to_the_app() {
        let s = spec(WITH_WORKER);
        let u = render_daemon_unit(&s, &s.daemons[1], &Layout::default());
        assert!(
            u.contains(
                "ExecStart=/usr/local/bin/comp-resume --nats-url nats://127.0.0.1:4222 \
                 --sink-url http://127.0.0.1:8016/x\n"
            ),
            "{u}"
        );
        assert!(!u.contains("--addr"), "it listens on nothing: {u}");
        assert!(u.contains("native worker beside park, listening on nothing"), "{u}");
        assert!(u.contains("BindsTo=comp-park.service"), "{u}");
        assert!(u.contains("MemoryDenyWriteExecute=yes") && u.contains("Restart=always"), "{u}");
        let d = render_daemon_unit(&s, &s.daemons[0], &Layout::default());
        assert!(d.contains("ExecStart=/usr/local/bin/comp-park --addr 127.0.0.1:8015\n"), "{d}");
    }

    #[test]
    fn the_apps_unit_is_given_egress_to_what_listens_and_not_to_a_worker() {
        let unit = render_unit(&spec(WITH_WORKER), &Layout::default());
        assert!(unit.contains("--egress 127.0.0.1:8015 --allow-private-egress"), "{unit}");
        assert_eq!(unit.matches("--egress").count(), 1, "nothing to reach at a worker: {unit}");
        let only = spec(
            "name = \"park\"\ndomain = \"p.example.com\"\nartifact = \"a.wasm\"\n\
             [[daemon]]\nname = \"resume\"\nextra_args = [\"--sink-url http://127.0.0.1:1/x\"]\n",
        );
        let u = render_unit(&only, &Layout::default());
        assert!(!u.contains("--egress") && !u.contains("--allow-private-egress"), "{u}");
    }

    #[test]
    fn a_worker_cannot_have_a_token_or_a_url_a_component_is_told() {
        let token = "name = \"park\"\ndomain = \"p.example.com\"\nartifact = \"a.wasm\"\n\
             [config]\nresume-token = \"t\"\n\
             [[daemon]]\nname = \"resume\"\ntoken = \"t\"\n";
        let err = toml::from_str::<Spec>(token).map(|s| check(&s)).expect("parses").unwrap_err();
        assert!(err.to_string().contains("no token for it to require"), "{err}");

        let url = "name = \"park\"\ndomain = \"p.example.com\"\nartifact = \"a.wasm\"\n\
             [config]\nresume-url = \"http://127.0.0.1:1\"\n\
             [[daemon]]\nname = \"resume\"\n";
        let err = toml::from_str::<Spec>(url).map(|s| check(&s)).expect("parses").unwrap_err();
        assert!(err.to_string().contains("listens on nothing"), "{err}");
    }

    #[test]
    fn a_daemon_named_like_its_app_gets_a_unit_of_its_own() {
        let s = spec(WITH_WORKER);
        assert_eq!(daemon_unit_name(&s, &s.daemons[0]), "comp-park-daemon.service");
        assert_eq!(daemon_unit_name(&s, &s.daemons[1]), "comp-resume.service");
        assert_eq!(
            unit_names(&s),
            ["comp-park.service", "comp-park-daemon.service", "comp-resume.service"]
        );
        let d = render_daemon_unit(&s, &s.daemons[0], &Layout::default());
        assert!(d.contains("BindsTo=comp-park.service") && d.contains("comp-park --addr"), "{d}");
        let fs = spec(
            "name = \"fs-watcher\"\ndomain = \"f.example.com\"\nartifact = \"a.wasm\"\n\
             [config]\nfswatch-url = \"http://127.0.0.1:8000\"\n\
             [[daemon]]\nname = \"fswatch\"\naddr = \"127.0.0.1:8000\"\n",
        );
        assert_eq!(daemon_unit_name(&fs, &fs.daemons[0]), "comp-fswatch.service");
    }

    #[test]
    fn two_units_that_would_share_a_file_are_refused() {
        let twice = "name = \"park\"\ndomain = \"p.example.com\"\nartifact = \"a.wasm\"\n\
             [[daemon]]\nname = \"resume\"\n[[daemon]]\nname = \"resume\"\n";
        let err = toml::from_str::<Spec>(twice).map(|s| check(&s)).expect("parses").unwrap_err();
        assert!(err.to_string().contains("would both be comp-resume.service"), "{err}");
        let relay = "name = \"park\"\ndomain = \"p.example.com\"\nartifact = \"a.wasm\"\n\
             [triggers]\n[[daemon]]\nname = \"park-relay\"\n";
        let err = toml::from_str::<Spec>(relay).map(|s| check(&s)).expect("parses").unwrap_err();
        assert!(err.to_string().contains("comp-park-relay.service"), "{err}");
    }

    #[test]
    fn no_committed_app_spec_renders_two_units_to_one_file() {
        let mut n = 0;
        for entry in std::fs::read_dir("../apps").expect("../apps") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap();
            let Ok(s) = toml::from_str::<Spec>(&src) else { continue }; // not an app spec
            check(&s).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let names = unit_names(&s);
            let unique: BTreeSet<&String> = names.iter().collect();
            assert_eq!(unique.len(), names.len(), "{}: {names:?}", path.display());
            n += 1;
        }
        assert!(n >= 10, "only {n} app specs read: the directory moved or the parse broke");
    }

    #[test]
    fn the_park_spec_renders_its_app_its_daemon_and_its_resumer() {
        let src = std::fs::read_to_string("../apps/park.toml").expect("../apps/park.toml");
        let s: Spec = toml::from_str(&src).expect("parses");
        check(&s).expect("valid");
        let l = Layout::default();
        assert_eq!(
            unit_names(&s),
            ["comp-park.service", "comp-park-daemon.service", "comp-resume.service"]
        );
        let app = render_unit(&s, &l);
        assert!(app.contains("comp-host") && app.contains("--egress 127.0.0.1:8015"), "{app}");
        assert_eq!(app.matches("--egress").count(), 1, "{app}");
        let resume = s.daemons.iter().find(|d| d.name == "resume").expect("a resume worker");
        assert_eq!(resume.addr, None);
        let u = render_daemon_unit(&s, resume, &l);
        assert!(!u.contains("--addr"), "{u}");
        assert!(u.contains("--sink-url ") && u.contains("--nats-url "), "{u}");
        let park = s.daemons.iter().find(|d| d.name == "park").expect("the park daemon");
        let nats = |d: &Daemon| d.extra_args.iter().find(|a| a.starts_with("--nats-url")).cloned();
        assert_eq!(nats(park), nats(resume), "comp-park and comp-resume must share a NATS");
    }

    /// A misspelt flag is a unit that restarts every two seconds. Skipped where unbuilt.
    #[test]
    fn every_flag_in_the_park_spec_exists_on_its_binaries() {
        let src = std::fs::read_to_string("../apps/park.toml").expect("../apps/park.toml");
        let s: Spec = toml::from_str(&src).expect("parses");
        let mut checked = 0;
        for d in &s.daemons {
            let bin = std::path::PathBuf::from(format!("../target/release/comp-{}", d.name));
            if !bin.exists() {
                eprintln!("skipping {}: no {} built", d.name, bin.display());
                continue;
            }
            let help = std::process::Command::new(&bin).arg("--help").output().expect("--help");
            let help = String::from_utf8_lossy(&help.stdout).to_string();
            let unit = render_daemon_unit(&s, d, &Layout::default());
            let exec = unit.lines().find(|l| l.starts_with("ExecStart=")).expect("an ExecStart");
            for flag in exec.split_whitespace().filter(|w| w.starts_with("--")) {
                assert!(help.contains(flag), "comp-{} has no {flag}\n--- help ---\n{help}", d.name);
            }
            checked += 1;
        }
        if checked == 0 {
            eprintln!("skipping entirely: no park binaries built under ../target/release");
        }
    }

    #[test]
    fn a_daemon_unit_binds_to_the_app_and_carries_its_allow_list() {
        let s = spec(
            "name = \"fs-watcher\"\ndomain = \"f.example.com\"\nartifact = \"a.wasm\"\n\
             [config]\nfswatch-url = \"http://127.0.0.1:8000\"\n\
             [[daemon]]\nname = \"fswatch\"\naddr = \"127.0.0.1:8000\"\n\
             allow_flag = \"allow-path\"\nallow = [\"/var/log\"]\n",
        );
        let d = &s.daemons[0];
        let u = render_daemon_unit(&s, d, &Layout::default());
        assert!(
            u.contains(
                "ExecStart=/usr/local/bin/comp-fswatch --addr 127.0.0.1:8000 --allow-path /var/log"
            ),
            "{u}"
        );
        assert!(u.contains("BindsTo=comp-fs-watcher.service"), "{u}");
        assert!(u.contains("MemoryDenyWriteExecute=yes"), "{u}");
        assert!(u.contains("ReadWritePaths=/var/log"), "{u}");
    }

    #[test]
    fn a_daemon_with_no_allow_list_gets_no_readwritepaths() {
        let s = spec(
            "name = \"docker-manager\"\ndomain = \"d.example.com\"\nartifact = \"a.wasm\"\n\
             [config]\ndocker-url = \"http://127.0.0.1:8002\"\n\
             [[daemon]]\nname = \"docker\"\naddr = \"127.0.0.1:8002\"\n",
        );
        let u = render_daemon_unit(&s, &s.daemons[0], &Layout::default());
        assert!(!u.contains("ReadWritePaths"), "{u}");
        assert!(u.contains("ExecStart=/usr/local/bin/holon-docker --addr 127.0.0.1:8002"), "{u}");
    }

    #[test]
    fn a_daemon_with_no_token_gets_no_credential_or_flag() {
        let s = spec(
            "name = \"docker-manager\"\ndomain = \"d.example.com\"\nartifact = \"a.wasm\"\n\
             [config]\ndocker-url = \"http://127.0.0.1:8002\"\n\
             [[daemon]]\nname = \"docker\"\naddr = \"127.0.0.1:8002\"\n",
        );
        let u = render_daemon_unit(&s, &s.daemons[0], &Layout::default());
        assert!(!u.contains("--token-file"), "{u}");
        assert!(!u.contains("LoadCredential"), "{u}");
    }

    #[test]
    fn a_daemons_token_reaches_it_through_a_credential_not_argv() {
        let s = spec(
            "name = \"docker-manager\"\ndomain = \"d.example.com\"\nartifact = \"a.wasm\"\n\
             [config]\ndocker-url = \"http://127.0.0.1:8002\"\ndocker-token = \"sekrit\"\n\
             [[daemon]]\nname = \"docker\"\naddr = \"127.0.0.1:8002\"\ntoken = \"sekrit\"\n",
        );
        let u = render_daemon_unit(&s, &s.daemons[0], &Layout::default());
        assert!(!u.contains("sekrit"), "the raw token must never appear in the unit: {u}");
        assert!(u.contains("--token-file %d/token"), "{u}");
        assert!(u.contains("LoadCredential=token:/etc/comp/docker-manager-docker.token"), "{u}");
    }

    #[test]
    fn a_daemon_token_disagreeing_with_its_config_is_refused() {
        let bad = "name = \"docker-manager\"\ndomain = \"d.example.com\"\nartifact = \"a.wasm\"\n\
             [config]\ndocker-url = \"http://127.0.0.1:8002\"\ndocker-token = \"one\"\n\
             [[daemon]]\nname = \"docker\"\naddr = \"127.0.0.1:8002\"\ntoken = \"two\"\n";
        let err = toml::from_str::<Spec>(bad).map(|s| check(&s)).expect("parses").unwrap_err();
        assert!(err.to_string().contains("must agree"), "{err}");
    }

    #[test]
    fn a_daemon_token_with_no_matching_config_key_is_refused() {
        let bad = "name = \"docker-manager\"\ndomain = \"d.example.com\"\nartifact = \"a.wasm\"\n\
             [config]\ndocker-url = \"http://127.0.0.1:8002\"\n\
             [[daemon]]\nname = \"docker\"\naddr = \"127.0.0.1:8002\"\ntoken = \"sekrit\"\n";
        let err = toml::from_str::<Spec>(bad).map(|s| check(&s)).expect("parses").unwrap_err();
        assert!(err.to_string().contains("no matching"), "{err}");
    }

    #[test]
    fn a_daemon_allow_value_with_a_percent_or_newline_is_refused() {
        for bad in ["allow = [\"/var/log/%h\"]\n", "allow = [\"/var/log\\nEnvironment=EVIL=1\"]\n"]
        {
            let spec_src = format!(
                "name = \"fs-watcher\"\ndomain = \"f.example.com\"\nartifact = \"a.wasm\"\n\
                 [config]\nfswatch-url = \"http://127.0.0.1:8000\"\n\
                 [[daemon]]\nname = \"fswatch\"\naddr = \"127.0.0.1:8000\"\nallow_flag = \"allow-path\"\n{bad}"
            );
            let err =
                toml::from_str::<Spec>(&spec_src).map(|s| check(&s)).expect("parses").unwrap_err();
            assert!(err.to_string().contains("newline"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn a_backend_without_a_url_is_refused() {
        let bad: Spec = toml::from_str(
            "name = \"g\"\ndomain = \"g.example.com\"\nartifact = \"a.wasm\"\nkv = \"redis\"\n",
        )
        .unwrap();
        assert!(check(&bad).is_err(), "redis with no kv_url must not render");
    }

    #[test]
    fn hostile_names_and_keys_are_refused() {
        for src in [
            "name = \"../etc\"\ndomain = \"g.example.com\"\nartifact = \"a.wasm\"\n",
            "name = \"Gate\"\ndomain = \"g.example.com\"\nartifact = \"a.wasm\"\n",
            "name = \"g\"\ndomain = \"\"\nartifact = \"a.wasm\"\n",
        ] {
            let s: Spec = toml::from_str(src).unwrap();
            assert!(check(&s).is_err(), "must refuse: {src}");
        }
        let s: Spec = toml::from_str(
            "name = \"g\"\ndomain = \"g.example.com\"\nartifact = \"a.wasm\"\n[config]\n\"A B\" = \"x\"\n",
        )
        .unwrap();
        assert!(check(&s).is_err());
    }

    /// A wrong flag fails only on the box, so ask the binary. Skipped where unbuilt.
    #[test]
    fn every_flag_we_emit_exists_on_comp_host() {
        let bin = std::path::Path::new("../target/release/comp-host");
        if !bin.exists() {
            eprintln!("skipping: no comp-host built at {}", bin.display());
            return;
        }
        let help = std::process::Command::new(bin).arg("--help").output().expect("run --help");
        let help = String::from_utf8_lossy(&help.stdout).to_string();

        let s = spec(
            r#"
name = "gate"
domain = "gate.example.com"
artifact = "a.wasm"
kv = "redis"
kv_url = "redis://127.0.0.1:6379"
static_dir = "ui/dist"
"#,
        );
        let unit = render_unit(&s, &Layout::default());
        let exec = unit.lines().find(|l| l.starts_with("ExecStart=")).expect("an ExecStart line");
        for flag in exec.split_whitespace().filter(|w| w.starts_with("--")) {
            assert!(help.contains(flag), "comp-host has no {flag}\n--- help ---\n{help}");
        }
        // The nats variant, with pooling off: `--no-pool` is the only pooling branch with a flag.
        let s2 = spec(
            "name = \"g\"\ndomain = \"g.example.com\"\nartifact = \"a.wasm\"\n\
             kv = \"nats\"\nkv_url = \"127.0.0.1:4222\"\npooling = false\n",
        );
        assert!(help.contains("--nats-url"), "{help}");
        let unit2 = render_unit(&s2, &Layout::default());
        assert!(unit2.contains("--nats-url 127.0.0.1:4222"));
        assert!(unit2.contains("--no-pool"), "pooling = false must reach the host: {unit2}");
        for flag in unit2
            .lines()
            .find(|l| l.starts_with("ExecStart="))
            .expect("an ExecStart line")
            .split_whitespace()
            .filter(|w| w.starts_with("--"))
        {
            assert!(help.contains(flag), "comp-host has no {flag}\n--- help ---\n{help}");
        }
    }

    #[test]
    fn every_flag_we_emit_for_a_daemon_exists_on_its_own_binary() {
        let daemons = [
            ("fswatch", Some("allow-path")),
            ("browser", Some("allow-host")),
            ("docker", None),
            ("clipboard", None),
            ("imageopt", Some("allow-path")),
            ("lanscan", Some("allow-cidr")),
            ("llmlocal", None),
            ("mdns", None),
            ("cron", None),
            ("uinotify", None),
            ("ffmpeg", Some("allow-path")),
            ("wireguard", Some("allow-interface")),
            ("media", None),
            ("vcs", Some("allow-path")),
        ];
        let s = spec(MINIMAL);
        let mut checked = 0;
        for (name, allow_flag) in daemons {
            let bin_holon = std::path::PathBuf::from(format!("../target/release/holon-{name}"));
            let bin_comp = std::path::PathBuf::from(format!("../target/release/comp-{name}"));
            let bin = if bin_holon.exists() { bin_holon } else { bin_comp };
            if !bin.exists() {
                eprintln!("skipping {name}: no {} built", bin.display());
                continue;
            }
            let help = std::process::Command::new(&bin).arg("--help").output().expect("run --help");
            let help = String::from_utf8_lossy(&help.stdout).to_string();
            assert!(help.contains("--addr"), "comp-{name} has no --addr\n{help}");

            let d = Daemon {
                name: name.into(),
                addr: Some("127.0.0.1:9".into()),
                allow_flag: allow_flag.map(String::from),
                allow: allow_flag.map(|_| vec!["x".into()]).unwrap_or_default(),
                extra_args: vec![],
                token: None,
            };
            let unit = render_daemon_unit(&s, &d, &Layout::default());
            assert!(!unit.contains("--token-file"), "no token configured: {unit}");
            assert!(help.contains("--token-file"), "comp-{name} has no --token-file\n{help}");
            let exec =
                unit.lines().find(|l| l.starts_with("ExecStart=")).expect("an ExecStart line");
            for flag in exec.split_whitespace().filter(|w| w.starts_with("--")) {
                assert!(help.contains(flag), "comp-{name} has no {flag}\n--- help ---\n{help}");
            }
            checked += 1;
        }
        if checked == 0 {
            eprintln!("skipping entirely: no comp-<daemon> binaries built under ../target/release");
        }
    }

    fn goald(toml_src: &str) -> GoaldSpec {
        let s: GoaldSpec = toml::from_str(toml_src).expect("parses");
        check_goald(&s).expect("valid");
        s
    }

    const MINIMAL_GOALD: &str = r#"
project = "holon"
checkout = "/srv/goald/holon"
repo = "me/holon"
"#;

    #[test]
    fn a_minimal_goald_spec_needs_three_lines() {
        let s = goald(MINIMAL_GOALD);
        assert_eq!(s.max_runs, 1);
        assert_eq!(s.poll, 15);
        assert!(!s.enforce_deepseek_offpeak, "off by default, like the flag it renders");
    }

    #[test]
    fn the_unit_runs_comp_goald_and_is_hardened() {
        let out = render_goald_unit(&goald(MINIMAL_GOALD), &Layout::default());
        assert!(out.contains("ExecStart=/usr/local/bin/comp-goald"), "{out}");
        assert!(
            out.contains("--project holon --checkout /srv/goald/holon --repo me/holon"),
            "{out}"
        );
        assert!(out.contains("Restart=always"));
        assert!(out.contains("DynamicUser=yes"));
        assert!(out.contains("ProtectSystem=strict"));
        assert!(out.contains("\nMemoryDenyWriteExecute=yes\n"), "{out}");
        assert!(!out.contains("--email"), "{out}");
        assert!(!out.contains("--password-file"), "{out}");
        assert!(!out.contains("--enforce-deepseek-offpeak"), "{out}");
        assert!(!out.contains("BindsTo"), "{out}");

        for line in out.lines() {
            if line.starts_with('#') || line.starts_with('[') || line.trim().is_empty() {
                continue;
            }
            assert!(
                !line.contains(" #"),
                "inline comment would become part of the value: {line:?}"
            );
        }
    }

    #[test]
    fn the_checkout_stays_writable_under_protectsystem_strict() {
        let out = render_goald_unit(&goald(MINIMAL_GOALD), &Layout::default());
        assert!(out.contains("ReadWritePaths=/srv/goald/holon"), "{out}");
    }

    #[test]
    fn off_peak_enforcement_and_holidays_reach_the_command_line() {
        let s = goald(&format!(
            "{MINIMAL_GOALD}enforce_deepseek_offpeak = true\nholidays = \"/etc/comp/cn-holidays.txt\"\n"
        ));
        let out = render_goald_unit(&s, &Layout::default());
        assert!(out.contains("--enforce-deepseek-offpeak"), "{out}");
        assert!(out.contains("--holidays /etc/comp/cn-holidays.txt"), "{out}");
    }

    #[test]
    fn goalrun_args_are_passed_through_after_a_bare_dash_dash() {
        let s = goald(&format!(
            "{MINIMAL_GOALD}goalrun_args = [\"--model\", \"deepseek-flash\", \"--branches\", \"4\"]\n"
        ));
        let out = render_goald_unit(&s, &Layout::default());
        assert!(out.contains("-- --model deepseek-flash --branches 4"), "{out}");
    }

    #[test]
    fn a_password_reaches_the_unit_through_a_credential_not_argv() {
        let s = goald(&format!(
            "{MINIMAL_GOALD}email = \"bot@holon.dev\"\npassword_file = \"/etc/comp/goald-holon.password\"\n"
        ));
        let out = render_goald_unit(&s, &Layout::default());
        assert!(out.contains("--email bot@holon.dev"), "{out}");
        assert!(out.contains("--password-file %d/password"), "{out}");
        assert!(out.contains("LoadCredential=password:/etc/comp/goald-holon.password"), "{out}");
    }

    #[test]
    fn email_without_a_password_file_is_refused() {
        let bad: GoaldSpec =
            toml::from_str(&format!("{MINIMAL_GOALD}email = \"bot@holon.dev\"\n")).unwrap();
        let err = check_goald(&bad).unwrap_err().to_string();
        assert!(err.contains("must be set together"), "{err}");
    }

    #[test]
    fn a_repo_that_is_not_owner_slash_name_is_refused() {
        let bad: GoaldSpec =
            toml::from_str("project = \"holon\"\ncheckout = \"/srv/holon\"\nrepo = \"holon\"\n")
                .unwrap();
        assert!(check_goald(&bad).is_err());
    }

    #[test]
    fn a_goalrun_arg_with_a_percent_or_newline_is_refused() {
        for bad in ["\"%h\"", "\"one\\ntwo\""] {
            let src = format!("{MINIMAL_GOALD}goalrun_args = [{bad}]\n");
            let s: GoaldSpec = toml::from_str(&src).unwrap();
            assert!(check_goald(&s).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_launchd_agent_runs_at_login_and_restarts_on_exit() {
        let out = render_goald_launchd(
            &goald(MINIMAL_GOALD),
            Path::new("/usr/local/bin"),
            Path::new("/tmp/goald-logs"),
        );
        assert!(out.starts_with("<?xml"), "{out}");
        assert!(out.contains("<string>dev.holon.goald.holon</string>"), "{out}");
        assert!(out.contains("<string>/usr/local/bin/comp-goald</string>"), "{out}");
        assert!(out.contains("<string>--project</string>"), "{out}");
        assert!(out.contains("<string>holon</string>"), "{out}");
        assert!(out.contains("<key>RunAtLoad</key>\n\t<true/>"), "{out}");
        assert!(out.contains("<key>KeepAlive</key>\n\t<true/>"), "{out}");
        assert!(out.contains("comp-goald-holon.out.log"), "{out}");
        assert!(out.contains("comp-goald-holon.err.log"), "{out}");
    }

    #[test]
    fn a_launchd_password_file_is_the_real_path_not_a_credential_indirection() {
        let s = goald(&format!(
            "{MINIMAL_GOALD}email = \"bot@holon.dev\"\npassword_file = \"/etc/holon/goald.password\"\n"
        ));
        let out = render_goald_launchd(&s, Path::new("/usr/local/bin"), Path::new("/tmp"));
        assert!(out.contains("<string>--password-file</string>"), "{out}");
        assert!(out.contains("<string>/etc/holon/goald.password</string>"), "{out}");
        assert!(!out.contains("%d/password"), "launchd has no credential dir: {out}");
        assert!(!out.contains("LoadCredential"), "{out}");
    }

    #[test]
    fn a_value_with_xml_metacharacters_cannot_break_out_of_its_string_element() {
        let s = goald(&format!("{MINIMAL_GOALD}goalrun_args = [\"--note\", \"a<b&c\"]\n"));
        let out = render_goald_launchd(&s, Path::new("/usr/local/bin"), Path::new("/tmp"));
        assert!(out.contains("<string>a&lt;b&amp;c</string>"), "{out}");
        assert!(!out.contains("<string>a<b&c</string>"), "{out}");
    }

    #[test]
    fn every_flag_we_emit_exists_on_comp_goald() {
        let bin = std::path::Path::new("../target/release/comp-goald");
        if !bin.exists() {
            eprintln!("skipping: no comp-goald built at {}", bin.display());
            return;
        }
        let help = std::process::Command::new(bin).arg("--help").output().expect("run --help");
        let help = String::from_utf8_lossy(&help.stdout).to_string();

        let s = goald(&format!(
            "{MINIMAL_GOALD}email = \"bot@holon.dev\"\npassword_file = \"/etc/comp/goald.password\"\n\
             enforce_deepseek_offpeak = true\nholidays = \"/etc/comp/cn-holidays.txt\"\n\
             goalrun_args = [\"--model\", \"deepseek-flash\"]\n"
        ));
        let unit = render_goald_unit(&s, &Layout::default());
        let exec = unit.lines().find(|l| l.starts_with("ExecStart=")).expect("an ExecStart line");
        // After the bare `--` come comp-goalrun's flags, not comp-goald's.
        let own = exec.split(" -- ").next().unwrap();
        for flag in own.split_whitespace().filter(|w| w.starts_with("--")) {
            assert!(help.contains(flag), "comp-goald has no {flag}\n--- help ---\n{help}");
        }
    }

    #[test]
    fn the_spec_carries_the_kubernetes_fields_it_does_not_use_yet() {
        let s = spec(
            "name = \"gate\"\ndomain = \"g.example.com\"\nartifact = \"a.wasm\"\n\
             components = [\"gate-domain\", \"record-store\"]\nstrategy = \"fused\"\n",
        );
        assert_eq!(s.components.len(), 2);
        assert_eq!(s.strategy.as_deref(), Some("fused"));
    }
}
