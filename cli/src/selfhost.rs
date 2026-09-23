//! `selfhost` — renders one app spec into the three files a box needs to serve it:
//! a systemd unit, an environment file, and a reverse-proxy site so the app gets its
//! own URL with automatic TLS.
//!
//! This is tier 1 of `docs/SELFHOST.md`: `comp-host` + systemd + Caddy (or Traefik),
//! no Kubernetes, no operator, no NATS. The design is deliberately the same shape as
//! the platform's `render.rs` — **a pure function from an app definition to the
//! artifacts a substrate needs** — because that is what makes the tiers progressive
//! rather than three separate products. Tier 3 renders the same spec into
//! `WorkloadDeployment` + `Service`; only the backend differs.
//!
//! Everything here is pure and tested. That is not ceremony: the k8s renderer's tests
//! caught three real bugs that manifest review had missed, and the failure modes here
//! are the same kind — a port collision, an unescaped value, a route pointing at the
//! wrong process.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use clap::ValueEnum;
use serde::Deserialize;

/// One application, as a person writes it. The only hand-authored file in the lane.
#[derive(Debug, Deserialize)]
pub struct Spec {
    /// DNS-label name. Becomes the unit name, the state directory and the route id.
    pub name: String,
    /// The URL this app answers on. The whole point of the tier — one hostname per
    /// app, TLS handled by the proxy.
    pub domain: String,
    /// The composed `.wasm` to serve, relative to the repo root
    /// (`cargo xtask compose <app>` produces these).
    pub artifact: String,

    /// Loopback port. Derived from the name when absent, so a spec need not carry
    /// bookkeeping — but an explicit one always wins, and `validate` refuses
    /// duplicates either way.
    #[serde(default)]
    pub port: Option<u16>,
    /// Where the app is mounted under `--router tailscale-serve`. Defaults to
    /// `/<name>`, because that router gets ONE hostname per machine and apps have
    /// to share it.
    ///
    /// A single-page app cannot take that default. Its `/api/...` calls and its
    /// `/assets/...` bundle are absolute, so mounted under `/events` every one of
    /// them misses — the page loads and nothing on it works, which is a worse
    /// failure than not deploying. Set `mount = "/"` on the one app that owns a
    /// box. Ignored by the other routers, which give a hostname per app and have
    /// no such choice to make.
    #[serde(default)]
    pub mount: Option<String>,
    /// `sqlite` (default: one file under `StateDirectory`, survives a restart),
    /// `memory` (lost on restart — honest only for caches), `redis` or `nats`.
    #[serde(default = "default_kv")]
    pub kv: String,
    /// Backend URL for `kv = "redis" | "nats"`. `sqlite` and `memory` need none.
    #[serde(default)]
    pub kv_url: Option<String>,
    /// Optional built SPA directory to serve for non-API GETs.
    #[serde(default)]
    pub static_dir: Option<String>,
    /// Pre-reserve instance slots. On by default, as on the host itself — `false`
    /// emits `comp-host --no-pool`. Measured at 3.1× with storage out of the way
    /// (ADR-0057), so turning it off wants a reason.
    #[serde(default = "default_true")]
    pub pooling: bool,

    /// Who can reach it. Defaults to `tailnet` — a forgotten field must not be the
    /// reason an app ends up on the public internet.
    ///
    /// * `tailnet` — bound to the box's Tailscale address only, HTTPS from Caddy's
    ///   own local CA (`tls internal`). No public DNS, no ACME, no DNS provider.
    /// * `public`  — bound to every interface, certificate from Let's Encrypt over
    ///   HTTP-01. For the few things strangers must reach.
    #[serde(default = "default_access")]
    pub access: String,

    /// `wasi:config` keys the component reads, delivered as `CFG_*` env.
    #[serde(default)]
    pub config: BTreeMap<String, String>,

    /// Component ids and strategy, for the Kubernetes lane (tier 3). Unused here,
    /// and present so that moving a spec up a tier is not a rewrite.
    #[serde(default)]
    pub components: Vec<String>,
    #[serde(default)]
    pub strategy: Option<String>,

    /// What drives this app's timers and topics, if anything does.
    ///
    /// `sched:timer`, `event:bus` and `cron:expr` are all PULL — each says in its
    /// own WIT that a relay must drive it. An app that imports one of them and has
    /// no `[triggers]` table is an app whose timers never fire, so this is where a
    /// deployment says "and run the poker too".
    #[serde(default)]
    pub triggers: Option<Triggers>,

    /// The native daemons behind ADR-0095's twelve host capabilities that this
    /// app's component dials over loopback HTTP (`comp-fswatch`, `comp-docker`,
    /// ...). A component with no daemon started answers `unavailable` forever —
    /// this is where a deployment says "and start those too".
    #[serde(default, rename = "daemon")]
    pub daemons: Vec<Daemon>,
}

/// One native daemon this app needs running alongside it.
///
/// Deliberately carries only what a DEPLOYMENT decides — where it listens and
/// what it may touch — not what the daemon defaults to on its own: that lives
/// in `reconciler/src/bin/<name>.rs` and this file has no business knowing all
/// twelve of them, the same reason `Spec` does not know what `comp-host`
/// itself defaults to.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Daemon {
    /// `comp-<name>` is the binary; `<name>-url` is the `[config]` key this
    /// daemon's own component reads. `check()` refuses a daemon whose `addr`
    /// disagrees with that key — the two are written in two different places
    /// on purpose (one is "where a process listens", the other is "what a
    /// wasm guest was told"), and nothing stops them drifting apart by hand.
    pub name: String,
    pub addr: String,
    /// The flag name for this daemon's own allow-list, if it has one —
    /// `allow-path`, `allow-host`, `allow-cidr`, `allow-interface`. Named
    /// rather than inferred: only the caller knows which one its daemon
    /// takes, and four of the twelve take none at all.
    #[serde(default)]
    pub allow_flag: Option<String>,
    #[serde(default)]
    pub allow: Vec<String>,
    /// Any other flags this daemon needs, verbatim and already `--flag value`
    /// shaped (`--ollama-url ...`, `--model ...`, `--max-width ...`).
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Shared secret the daemon requires as `Authorization: Bearer <token>`.
    /// Loopback binding alone is not a boundary — any other local process can
    /// otherwise reach it with no caller-identity check at all. Absent means
    /// the daemon runs with no check, same `check()`-enforced agreement with
    /// `[config]`'s `<name>-token` as `addr` has with `<name>-url`.
    #[serde(default)]
    pub token: Option<String>,
}

/// The relay's half of an app spec.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Triggers {
    /// The path to POST. Every app in this tree that drains anything calls it
    /// `/internal/pump` (`saga-domain`, the four `eshop-*` services), so that is the
    /// default and naming it is for the app that chose otherwise.
    #[serde(default = "default_pump")]
    pub pump: String,
    /// Seconds between sweeps. The completeness path: it must be short enough that a
    /// missed push is a delay rather than a stall.
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
    // sqlite, not memory: `Restart=always` means restarts are routine, so a default
    // that silently loses data is the wrong one. comp-host puts the file in
    // $STATE_DIRECTORY, which the unit already declares — so this needs no path.
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

/// Which background-job mechanism `comp-goald` gets rendered for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GoaldFormat {
    /// A hardened unit for tier 1's own box: `docs/SELFHOST.md`'s
    /// `comp-host` + systemd + Caddy.
    Systemd,
    /// A `launchd` LaunchAgent for a developer's own Mac — no systemd there.
    Launchd,
}

// ---- pure rendering ---------------------------------------------------------

/// Where an app's files live on the box. One prefix, so removing an app is
/// removing four paths rather than remembering four conventions.
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

/// A stable loopback port for an app that did not name one.
///
/// Deterministic so that rendering twice produces the same unit, and re-deploying
/// does not silently move an app to a new port while the proxy still points at the
/// old one. Collisions are possible in principle and `validate` is what catches
/// them — a hash is a convenience, not a registry.
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
    // A `.ts.net` name is issued by Tailscale and resolves only inside the tailnet;
    // asking for a public Let's Encrypt certificate for it cannot work.
    if spec.access == "public" && spec.domain.ends_with(".ts.net") {
        bail!(
            "domain {:?} is a Tailscale name, which cannot be reached or certified publicly — use access = \"tailnet\"",
            spec.domain
        );
    }
    if !matches!(spec.kv.as_str(), "memory" | "sqlite" | "redis" | "nats") {
        bail!("kv must be memory|sqlite|redis|nats, got {:?}", spec.kv);
    }
    // Only the network backends need an address. sqlite derives its path from the
    // unit's StateDirectory, which is the point of it.
    if matches!(spec.kv.as_str(), "redis" | "nats") && spec.kv_url.is_none() {
        bail!("kv = {:?} needs kv_url", spec.kv);
    }
    for k in spec.config.keys() {
        // These become `CFG_<UPPER_SNAKE>` env names; anything else would silently
        // produce a variable the component cannot read.
        if k.is_empty()
            || !k.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            bail!("config key {k:?} must be lowercase-with-dashes");
        }
    }
    for d in &spec.daemons {
        if !d.addr.contains(':') {
            bail!("daemon {:?} addr {:?} is not host:port", d.name, d.addr);
        }
        // `render_daemon_unit` interpolates these straight into `ExecStart=`.
        // Operator-authored today, not attacker input — but `%` is a systemd
        // specifier and a newline would inject a second directive, so a spec
        // that somehow got machine-generated from something less trusted
        // fails here rather than writing a unit that means something else.
        let clean = |v: &str| !v.contains('%') && !v.contains('\n');
        if !clean(&d.name) || !d.allow.iter().all(|a| clean(a)) || !d.extra_args.iter().all(|a| clean(a))
        {
            bail!("daemon {:?}: name/allow/extra_args may not contain '%' or a newline", d.name);
        }
        // The unit binds `d.addr`; the component reads `<name>-url` from
        // `[config]` to find it. Nothing enforces those agree by construction
        // — they are written in two different tables for two different
        // readers — so this is the one check standing between "the daemon is
        // up" and "the component was never told where".
        let key = format!("{}-url", d.name);
        match spec.config.get(&key) {
            Some(v) if *v == format!("http://{}", d.addr) => {}
            Some(v) => bail!(
                "daemon {:?} binds {:?} but [config] {key} = {v:?} — they must agree",
                d.name,
                d.addr
            ),
            None => bail!("daemon {:?} has no matching [config] {key}", d.name),
        }
        // Same agreement as `addr`/`<name>-url`, for the same reason: `token`
        // is what the unit passes the daemon on its command line, `<name>-token`
        // is what `[config]` hands the wasm guest — nothing but this check
        // stops the two drifting apart by hand. Absent on both sides is fine
        // (the daemon runs with no auth, its own loud warning says so); present
        // on one side and not the other is a component that will send a
        // header the daemon does not expect, or a daemon that will reject a
        // component sending none.
        if let Some(want) = &d.token {
            let key = format!("{}-token", d.name);
            match spec.config.get(&key) {
                Some(v) if v == want => {}
                Some(v) => bail!(
                    "daemon {:?} token {:?} but [config] {key} = {v:?} — they must agree",
                    d.name,
                    want
                ),
                None => bail!("daemon {:?} sets a token but has no matching [config] {key}", d.name),
            }
        }
    }
    Ok(())
}

/// The app's `wasi:config`, as `comp-host --config-file` reads it.
///
/// This rendered `CFG_GRACE_PERIOD_SECS=5` and systemd loaded it as an environment
/// variable, which is how `comp-host` used to read config. It does not any more,
/// and the reason is in `host/src/main.rs`: in a process shared by every tenant on
/// a node, process environment as config is a cross-tenant read by construction —
/// one `getenv` and every app sees every other app's knobs, secrets included. So
/// the scrape was removed and config moved to the start command.
///
/// The renderer was not moved with it. Every `[config]` block in every spec has
/// been written to a file nothing reads since: `paste`'s `ticket-ttl`,
/// `eshop-ordering`'s `grace-period-secs`, and the key that found this — an app
/// deployed with `organizer-emails` set, where nobody could open an event because
/// the component saw no config at all. Silent, because a missing key looks exactly
/// like a key whose value is the default.
///
/// `key = value` and `--config-file`, not repeated `--config` flags: config holds
/// things like an admin's address, and argv is world-readable in `ps`. This file is
/// 0600 and owned by root.
/// Writes a file `LoadCredential` is meant to read as root and hand to a
/// service under 0600, and gives it that permission itself rather than
/// leaving it to the deploy step's `LoadCredential` doc comment to be true by
/// assertion — `render_unit`'s own comment claimed "the config file is 0600
/// and owned by root" while nothing in this codebase ever set that
/// permission; a rendered file inherited whatever the umask of whoever ran
/// `render` happened to be, typically world-readable. Owner-root still
/// depends on where a deploy step copies this file to, which is genuinely
/// outside what a local `render` can promise — 0600 on the file `render`
/// itself writes is not.
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
        // One line per key, so a newline in a value would silently become a second
        // key. It is the one character that has to go.
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
        // comp-host names the URL flag after the backend.
        let flag = if spec.kv == "redis" { "--redis-url" } else { "--nats-url" };
        args.push_str(&format!(" {flag} {url}"));
    }
    if spec.static_dir.is_some() {
        args.push_str(&format!(" --static-dir {}/{}/static", layout.app_dir.display(), app));
    }
    // The polarity flipped under this: pooling became the default and the flag
    // became `--no-pool` (ADR-0057), so emitting `--pool` wrote a unit `comp-host`
    // exits on. The test below catches exactly this, and only on a box that has the
    // host built — which is why it went unnoticed.
    if !spec.pooling {
        args.push_str(" --no-pool");
    }
    // comp-host denies all outbound HTTP by default, and its address check
    // refuses loopback — so a component whose daemon was started beside it
    // still answered `unavailable` without these. The allow-list is only the
    // daemon addresses (IP literals, `check()` insists on host:port), so
    // `--allow-private-egress` opens nothing a name could resolve into.
    for d in &spec.daemons {
        args.push_str(&format!(" --egress {}", d.addr));
    }
    if !spec.daemons.is_empty() {
        args.push_str(" --allow-private-egress");
    }

    let mut s = String::new();
    s.push_str("# Generated by selfhost — do not edit; edit the app spec and re-deploy.\n");
    s.push_str(&format!("[Unit]\nDescription=comp-host: {app} ({})\n", spec.domain));
    s.push_str("After=network-online.target\nWants=network-online.target\n\n");
    s.push_str("[Service]\nType=simple\n");
    // Through a systemd CREDENTIAL, not a path.
    //
    // The config file is 0600 and owned by root, and this unit runs `DynamicUser=yes`
    // — so the process cannot open it. `EnvironmentFile` could, because systemd reads
    // that as root BEFORE dropping privileges; `--config-file` is opened by the
    // process afterwards, and pointing it straight at /etc/comp gave
    // `Permission denied (os error 13)` in a restart loop.
    //
    // `LoadCredential` is the mechanism for exactly this: systemd reads the file as
    // root and drops a copy into a private directory the service can read and nobody
    // else can. `%d` expands to that directory. The alternative — `--config k=v` on
    // ExecStart — puts every value in `ps` for every user on the box.
    if !spec.config.is_empty() {
        args.push_str(" --config-file %d/config");
    }
    if !spec.config.is_empty() {
        s.push_str(&format!(
            "LoadCredential=config:{}/{}.env\n",
            layout.env_file.display(),
            app
        ));
    }
    s.push_str(&format!("ExecStart={} {}\n", layout.bin.display(), args));
    s.push_str("Restart=always\nRestartSec=2\n");
    // Hardening. Cheap, and this process runs code from a wasm artifact on a box
    // reachable from the internet — `DynamicUser` gives it a throwaway uid and
    // `StateDirectory` the one writable path it is allowed.
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
    // On its OWN line: systemd has no inline comments, so `key=value # why` makes
    // the comment part of the value and the directive invalid.
    s.push_str("# wasmtime JITs, so it needs W^X — this one cannot be tightened.\n");
    s.push_str("MemoryDenyWriteExecute=no\n");
    s.push_str("\n[Install]\nWantedBy=multi-user.target\n");
    s
}

/// The per-app URL. Caddy obtains and renews the certificate on its own, which is
/// the entire reason to put a proxy in front rather than binding `:443` per app.
/// A Caddy site that fronts `comp-ingress` with TLS.
///
/// One site for the whole lattice rather than one per app: the ingress already
/// routes by `Host` header from inventory, so Caddy needs to know nothing about
/// which apps exist and never needs regenerating when one is deployed. That is the
/// difference between this and `render_route`, which fronts a single app on a
/// single box.
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

/// The hardening every rendered unit shares except `comp-host`'s own, which
/// JITs and so cannot take the one line this deliberately leaves out —
/// `MemoryDenyWriteExecute=yes`, appended separately by each caller since
/// that is the one thing that differs between them.
///
/// `restart_sec` is the other difference worth naming: 2s suits a sidecar
/// that should come back fast, but `comp-goald` passes a longer one whose
/// own doc names why (a platform session lasts ~1h; a tight restart loop
/// would just spend CPU while that same clock keeps ticking).
fn hardening(restart_sec: u64) -> String {
    format!(
        "Restart=always\nRestartSec={restart_sec}\nDynamicUser=yes\nNoNewPrivileges=yes\n\
         PrivateTmp=yes\nPrivateDevices=yes\nProtectSystem=strict\nProtectHome=yes\n\
         ProtectKernelTunables=yes\nProtectControlGroups=yes\n\
         RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX\nRestrictNamespaces=yes\n\
         LockPersonality=yes\n"
    )
}

/// The relay unit for an app that declares `[triggers]`.
///
/// A SECOND unit rather than a second process inside the app's: it restarts
/// independently, it is `systemctl stop`-able on its own when a pump is misbehaving,
/// and its journal is separate — the same argument tier 1 makes for one unit per app.
///
/// It dials `127.0.0.1:<port>` directly, not the app's public hostname. The proxy is
/// for the outside; a poker on the same box has no reason to leave it, and going out
/// through Caddy would make an internal endpoint reachable from wherever the route
/// is reachable from.
pub fn render_relay_unit(spec: &Spec, t: &Triggers, l: &Layout) -> String {
    let mut s = String::from(
        "# Generated by selfhost — do not edit; edit the app spec and re-deploy.\n",
    );
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
    // No lease: tier 1 is one box running one copy of one app, so there is nothing to
    // elect between. The lattice lane is where two relays can exist and where the
    // lease stops them racing a consumer group's offset.
    s.push_str(&hardening(2));
    // Unlike comp-host, this one JITs nothing — it is an HTTP client with a clock.
    s.push_str("MemoryDenyWriteExecute=yes\n");
    s.push_str("\n[Install]\nWantedBy=multi-user.target\n");
    s
}

/// The unit for one native daemon behind ADR-0095 (see `Daemon`'s own doc).
///
/// A SECOND unit per daemon, same reasoning as the relay: `container-docker`
/// and `ui-notifier` do not deserve the same blast radius, and one shared
/// runner for all twelve would give them one. `BindsTo`/`After` the app unit
/// for the same reason the relay does — tier 1 is one box running one copy of
/// one app, so a daemon this app started has no reason to outlive it.
pub fn render_daemon_unit(spec: &Spec, d: &Daemon, l: &Layout) -> String {
    let bin_dir =
        l.bin.parent().map(|p| p.display().to_string()).unwrap_or_else(|| "/usr/local/bin".into());
    let mut args = format!("--addr {}", d.addr);
    if let Some(flag) = &d.allow_flag {
        for v in &d.allow {
            args.push_str(&format!(" --{flag} {v}"));
        }
    }
    for a in &d.extra_args {
        args.push_str(&format!(" {a}"));
    }
    // `--token-file %d/token`, not `--token <value>`: the same reason
    // `comp-host`'s own unit reads its config through `LoadCredential` rather
    // than argv — a value on the command line is `ps`-readable by any local
    // user, which is most of what a token exists to close.
    if d.token.is_some() {
        args.push_str(" --token-file %d/token");
    }

    let mut s = String::from("# Generated by selfhost — do not edit; edit the app spec and re-deploy.\n");
    s.push_str(&format!(
        "[Unit]\nDescription=comp-{}: native daemon behind {} (ADR-0095)\n\
         After={unit}\nBindsTo={unit}\n\n[Service]\nType=simple\n",
        d.name,
        spec.name,
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
    s.push_str(&format!("ExecStart={bin_dir}/comp-{} {args}\n", d.name));
    s.push_str(&hardening(2));
    // None of the twelve JIT — they are plain Rust binaries, not a wasmtime
    // host — so unlike comp-host's unit this one is not the exception.
    s.push_str("MemoryDenyWriteExecute=yes\n");
    // `ProtectSystem=strict` makes everything but a few pseudo-filesystems
    // read-only. A daemon whose allow-list names real directories on disk
    // (fs-watcher/image-optimizer/video-ffmpeg's `--allow-path`) needs those
    // specific paths writable back through it, or its own allow-list would
    // permit a read/write that the unit itself refuses first.
    if d.allow_flag.as_deref() == Some("allow-path") && !d.allow.is_empty() {
        s.push_str(&format!("ReadWritePaths={}\n", d.allow.join(" ")));
    }
    s.push_str("\n[Install]\nWantedBy=multi-user.target\n");
    s
}

// ---- comp-goald: a background job for the agentic loop's own daemon --------

/// One `comp-goald` deployment: a process that continuously drains ONE
/// project's goal queue (ADR-0082 + ADR-0096's "no cron was ever written").
///
/// Deliberately its own spec, not a field on [`Spec`]: `comp-goald` is not
/// scoped to a deployed app at all — it watches a git repository's `.comp/`
/// goals and opens PRs against it, which has no `domain`, no `artifact`, and
/// no reason to share a lifecycle with anything `comp-host` serves.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoaldSpec {
    /// The project whose queue this drains — `holon goal ls <project>`.
    pub project: String,
    /// A local checkout of that project's repository, on the box the unit runs on.
    pub checkout: String,
    /// `owner/name` of the repository the PRs open on.
    pub repo: String,
    #[serde(default)]
    pub platform_url: Option<String>,
    /// The account to sign back in as when the platform session expires
    /// (~1h) — required together with `password_file`, or the daemon dies
    /// silently on the first renewal (`comp-goald`'s own doc comment).
    #[serde(default)]
    pub email: Option<String>,
    /// A path on the box holding that account's password — never the value
    /// itself. Read into the unit through `LoadCredential`, the same
    /// reasoning as `Daemon::token`: a value on `ExecStart` is `ps`-readable
    /// by any local user, and this unit runs `DynamicUser=yes`.
    #[serde(default)]
    pub password_file: Option<String>,
    #[serde(default = "default_max_runs")]
    pub max_runs: u32,
    #[serde(default = "default_poll")]
    pub poll: u64,
    /// Hold a DeepSeek-backed run until DeepSeek's off-peak window opens
    /// (`comp_reconciler::offpeak`) instead of spending it at up to 2x the
    /// price. Off by default — see `comp-goald --help`.
    #[serde(default)]
    pub enforce_deepseek_offpeak: bool,
    /// A file of `YYYY-MM-DD` off-peak-all-day lines. Only meaningful with
    /// `enforce_deepseek_offpeak = true`.
    #[serde(default)]
    pub holidays: Option<String>,
    /// Flags for `comp-goalrun`, handed through verbatim — the model, the
    /// budget, the pool, the branch count. `comp-goald` grows no opinion
    /// about any of these, and neither does this renderer.
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
    // `comp-goald` itself: `(Some(email), Some(password_file))` is the only
    // combination that renews a session; either alone silently becomes
    // `login: None` and the daemon dies on the first renewal instead of
    // refusing to start.
    if spec.email.is_some() != spec.password_file.is_some() {
        bail!("email and password_file must be set together, or not at all");
    }
    // These are interpolated straight into `ExecStart=`, same hazard
    // `render_daemon_unit`'s own check guards against.
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

/// The unit for one `comp-goald` deployment.
///
/// Not `BindsTo`/`After` any app unit — unlike the relay or the twelve
/// ADR-0095 daemons, this is not a sidecar to something `comp-host` serves.
/// It outlives any single app's lifecycle, so it gets the same
/// `After=network-online.target` as `render_unit`'s own `comp-host` unit.
pub fn render_goald_unit(spec: &GoaldSpec, l: &Layout) -> String {
    let bin_dir =
        l.bin.parent().map(|p| p.display().to_string()).unwrap_or_else(|| "/usr/local/bin".into());

    let mut args = format!(
        "--project {} --checkout {} --repo {}",
        spec.project, spec.checkout, spec.repo
    );
    if let Some(url) = &spec.platform_url {
        args.push_str(&format!(" --platform-url {url}"));
    }
    if let Some(email) = &spec.email {
        args.push_str(&format!(" --email {email}"));
    }
    if spec.password_file.is_some() {
        // Through the credential systemd drops in, not the real path — the
        // same indirection `render_daemon_unit` uses for a daemon's token.
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

    let mut s = String::from("# Generated by selfhost — do not edit; edit the goald spec and re-deploy.\n");
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
    // The daemon's own doc comment names the failure this survives: a run
    // that outlives a one-hour platform session with no --email/--password-file
    // to renew it dies, and a restart is the whole recovery story tier 1 has.
    s.push_str(&hardening(5));
    // A plain Rust binary that shells out to comp-goalrun and comp-checks —
    // no wasmtime host inside it — so unlike comp-host's unit this one has no
    // reason to leave W^X open.
    s.push_str("MemoryDenyWriteExecute=yes\n");
    // The checkout is a real working tree comp-goalrun commits branches into
    // and comp-checks materialises candidates under — `ProtectSystem=strict`
    // would make that read-only and every run would fail before a model was
    // ever called.
    s.push_str(&format!("ReadWritePaths={}\n", spec.checkout));
    s.push_str("\n[Install]\nWantedBy=multi-user.target\n");
    s
}

/// The `comp-goald` flags common to every deployment format, already split
/// into argv-style tokens, in the order `comp-goald --help` lists them.
/// `password_value` is what `--password-file` should point AT — the real
/// path for launchd (a LaunchAgent already runs at its owner's own uid, so it
/// can read a file that uid owns directly), or `%d/password` for systemd's
/// `render_goald_unit`, which needs the credential indirection instead.
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

/// A macOS `launchd` LaunchAgent for `comp-goald` — the background-job
/// mechanism on a box with no systemd, which every `comp-goald` run this
/// project has ever made has actually been: a person's own Mac, in a
/// foreground terminal tab, because nothing supervised it.
///
/// Runs at the signed-in user's own uid — no `DynamicUser`, no
/// `LoadCredential`: a personal LaunchAgent already has exactly the access
/// that uid does, which is also all `comp-goald` itself ever assumed.
/// `RunAtLoad` starts it at login; `KeepAlive` restarts it if it exits.
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
        // No `tls` directive: Caddy's default IS ACME, and spelling it out wrongly
        // is how you end up with a self-signed cert on a public name.
        assert!(!c.contains("tls internal"));
    }

    #[test]
    fn the_tailnet_front_is_unreachable_publicly_and_still_a_secure_context() {
        // Both halves matter. `bind` alone leaves it on http; `tls internal` alone
        // leaves it listening on every interface.
        let c = render_ingress_route("lattice.ts.net", "127.0.0.1:8088", true);
        assert!(c.contains("bind {$TS_IP}"), "must not listen publicly");
        assert!(c.contains("tls internal"), "passkeys need a secure context");
    }

    #[test]
    fn one_site_fronts_the_whole_lattice_not_one_app() {
        // The ingress routes by Host from inventory, so this file never mentions an
        // app and never needs regenerating when one is deployed.
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
        // Durable by default: a spec that says nothing must not lose data on the
        // first restart, and `Restart=always` makes restarts routine.
        assert_eq!(s.kv, "sqlite");
        assert!(
            s.pooling,
            "pooling defaults on — it is what makes per-request instantiation cheap"
        );
        assert_eq!(port_of(&s), derived_port("gate"));
    }

    #[test]
    fn the_derived_port_is_stable_and_in_range() {
        // Stability is the point: a re-render must not move a running app to a new
        // port while the proxy still points at the old one.
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
        // MINIMAL declares no config, so no --config-file: a unit naming a file the
        // deploy had no reason to write is a unit that fails to start.
        assert!(!out.contains("--config-file"), "{out}");
        // Hardening, and the one exception that has to be there.
        assert!(out.contains("DynamicUser=yes"));
        assert!(out.contains("ProtectSystem=strict"));
        assert!(out.contains("NoNewPrivileges=yes"));
        assert!(out.contains("StateDirectory=comp/gate"));
        assert!(out.contains("\nMemoryDenyWriteExecute=no\n"), "wasmtime JITs: {out}");

        // systemd has no inline comments: `key=value # why` would make the comment
        // part of the value. Every directive line must be bare.
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

    /// A spec with config gets a unit that READS it.
    ///
    /// The unit carried `EnvironmentFile` and no `--config-file`, and comp-host had
    /// stopped reading `CFG_*` from the environment — so every `[config]` block in
    /// every spec was written to a file nothing opened. Nothing failed: a key that
    /// is never read looks exactly like a key left at its default, which is why it
    /// survived until an app needed one to decide who may open an event.
    #[test]
    fn config_reaches_the_host_through_the_start_command() {
        let with = spec(
            "name = \"gate\"\ndomain = \"g.example.com\"\nartifact = \"a.wasm\"\n\
             [config]\nk = \"v\"\n",
        );
        let unit = render_unit(&with, &Layout::default());
        // Through a credential, because the unit is DynamicUser and the file is
        // root-owned 0600 — pointing --config-file at /etc/comp directly is a
        // restart loop on `Permission denied`.
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
        // `comp-host --config-file` reads `key = value`. It used to be
        // `CFG_GRACE_PERIOD_SECS=5` in the process environment, and this test
        // asserted that faithfully for as long as the host had stopped reading it.
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
        // Without the bind it would listen on the VPS's public interface too, which
        // is the difference between "private" and "accidentally on the internet".
        assert!(out.contains("bind {$TS_IP}"), "{out}");
        // No ACME, no DNS provider — the constraint that ruled out DNS-01.
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
        // Tailscale issues the name and it resolves only inside the tailnet, so a
        // public certificate for it is impossible. Refuse rather than fail at ACME.
        let s: Spec = toml::from_str(
            "name = \"g\"\ndomain = \"box.tail1234.ts.net\"\nartifact = \"a.wasm\"\n\
             access = \"public\"\n",
        )
        .unwrap();
        let err = check(&s).unwrap_err().to_string();
        assert!(err.contains("Tailscale name"), "{err}");
        // ...and the same name is fine when it is honest about being private.
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
        // One hostname per machine, so apps are distinguished by path — and the file
        // says so, because it will break an app that assumes it lives at `/`.
        assert!(out.contains("--set-path /gate"), "{out}");
        assert!(out.contains(&format!("http://127.0.0.1:{}", port_of(&spec(MINIMAL)))));
        assert!(out.starts_with("#!/usr/bin/env bash"), "it is a script, not config");
    }

    /// `mount = "/"` for the app that owns the box.
    ///
    /// The default `/<name>` is right when several apps share one Tailscale
    /// hostname and wrong for every SPA: `/api/...` and `/assets/...` are absolute,
    /// so under `/events` the page loads and nothing on it works. That failure is
    /// silent in a way "no route" is not, which is why the option exists.
    #[test]
    fn an_app_that_owns_the_box_can_be_mounted_at_the_root() {
        let spec: Spec = toml::from_str(
            "name = \"events\"\ndomain = \"malna.tail3a9c.ts.net\"\n\
             artifact = \"a.wasm\"\nport = 3230\nmount = \"/\"\n",
        )
        .unwrap();
        let out = render_route(&spec, Router::TailscaleServe);
        assert!(out.contains("--set-path / http://127.0.0.1:3230"), "{out}");
        // And the URL it prints has no path glued on the end.
        assert!(!out.contains("')/events"), "{out}");

        // Absent, the default is still one path per app.
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
        // No --sqlite-path: comp-host reads $STATE_DIRECTORY, which this unit already
        // declares, and under DynamicUser that path is private to the app.
        assert!(!unit.contains("--sqlite-path"), "{unit}");
        assert!(unit.contains("StateDirectory=comp/gate"), "{unit}");
    }

    #[test]
    fn an_app_with_no_triggers_gains_no_poker() {
        // Every app in the tree would otherwise get a process poking it forever.
        assert!(spec(MINIMAL).triggers.is_none());
    }

    #[test]
    fn a_relay_unit_pokes_the_loopback_port_and_not_the_public_hostname() {
        let s = spec(&format!("{}\n[triggers]\n", MINIMAL));
        let t = s.triggers.as_ref().unwrap();
        // The defaults are the convention: saga-domain and the four eshop services
        // all export exactly this path.
        assert_eq!(t.pump, "/internal/pump");
        let u = render_relay_unit(&s, t, &Layout::default());
        assert!(
            u.contains(&format!("--target http://127.0.0.1:{}/internal/pump", port_of(&s))),
            "{u}"
        );
        // Going out through the proxy would make an internal endpoint reachable from
        // wherever the route is.
        assert!(!u.contains(&s.domain), "the relay must not dial the public name: {u}");
        // It binds to the app's lifecycle: pointless before it starts.
        assert!(u.contains("BindsTo=comp-gate.service"), "{u}");
        // It JITs nothing, unlike comp-host — so this one CAN be tightened.
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

    /// The app's unit must let the component reach its daemon: comp-host is
    /// default-deny on egress and refuses loopback without the private flag.
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
        assert!(u.contains("ExecStart=/usr/local/bin/comp-fswatch --addr 127.0.0.1:8000 --allow-path /var/log"), "{u}");
        assert!(u.contains("BindsTo=comp-fs-watcher.service"), "{u}");
        // A daemon is a plain binary, not a wasmtime host — unlike comp-host's
        // unit, this one has no reason to leave W^X open.
        assert!(u.contains("MemoryDenyWriteExecute=yes"), "{u}");
        // ProtectSystem=strict would otherwise refuse the very path the
        // daemon's own --allow-path just granted.
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
        assert!(u.contains("ExecStart=/usr/local/bin/comp-docker --addr 127.0.0.1:8002"), "{u}");
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

    /// `--token`, like `--config`, must never reach `ps` — a value on the
    /// command line is `ps`-readable by any local user, exactly the reason
    /// `comp-host`'s own config goes through `LoadCredential` instead.
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
        assert!(
            u.contains("LoadCredential=token:/etc/comp/docker-manager-docker.token"),
            "{u}"
        );
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

    /// `render_daemon_unit` writes these straight into `ExecStart=`, where `%`
    /// is a systemd specifier and a newline would inject a second directive.
    #[test]
    fn a_daemon_allow_value_with_a_percent_or_newline_is_refused() {
        for bad in [
            "allow = [\"/var/log/%h\"]\n",
            "allow = [\"/var/log\\nEnvironment=EVIL=1\"]\n",
        ] {
            let spec_src = format!(
                "name = \"fs-watcher\"\ndomain = \"f.example.com\"\nartifact = \"a.wasm\"\n\
                 [config]\nfswatch-url = \"http://127.0.0.1:8000\"\n\
                 [[daemon]]\nname = \"fswatch\"\naddr = \"127.0.0.1:8000\"\nallow_flag = \"allow-path\"\n{bad}"
            );
            let err = toml::from_str::<Spec>(&spec_src).map(|s| check(&s)).expect("parses").unwrap_err();
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
        // A config key that would not survive the CFG_ translation.
        let s: Spec = toml::from_str(
            "name = \"g\"\ndomain = \"g.example.com\"\nartifact = \"a.wasm\"\n[config]\n\"A B\" = \"x\"\n",
        )
        .unwrap();
        assert!(check(&s).is_err());
    }

    /// The renderer's flags must exist on the binary it is writing a unit for.
    ///
    /// No pure test can know this, and getting it wrong produces a unit that fails
    /// only on the box: the first draft emitted `--static` and `--pooling`, where
    /// `comp-host` wants `--static-dir` and `--no-pool`. So ask the binary.
    ///
    /// It caught the same class a second time when pooling became the default and
    /// `--pool` stopped existing — a unit that systemd would have refused to start.
    ///
    /// Skipped when the host has not been built, because a renderer test should not
    /// require a 30 MB compile — but it runs on any machine that has one.
    #[test]
    fn every_flag_we_emit_exists_on_comp_host() {
        let bin = std::path::Path::new("../host/target/release/comp-host");
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
        // And the nats variant, which the redis spec above does not exercise. It
        // also turns pooling OFF, because that is the only branch that emits a flag
        // now — with pooling on, the unit says nothing, so the default path cannot
        // catch a rename here a second time.
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
        // The thirteen (ADR-0095's twelve plus ADR-0098's comp-media), and the allow-list flag (if any) each one actually takes —
        // read from `reconciler/src/bin/<name>.rs`'s own `Args`, not invented
        // here. A daemon renamed or a flag renamed on its own binary would
        // otherwise only be caught by a unit that fails to start on a real box.
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
        ];
        let s = spec(MINIMAL);
        let mut checked = 0;
        for (name, allow_flag) in daemons {
            let bin =
                std::path::PathBuf::from(format!("../reconciler/target/release/comp-{name}"));
            if !bin.exists() {
                eprintln!("skipping {name}: no {} built", bin.display());
                continue;
            }
            let help = std::process::Command::new(&bin).arg("--help").output().expect("run --help");
            let help = String::from_utf8_lossy(&help.stdout).to_string();
            assert!(help.contains("--addr"), "comp-{name} has no --addr\n{help}");

            let d = Daemon {
                name: name.into(),
                addr: "127.0.0.1:9".into(),
                allow_flag: allow_flag.map(String::from),
                allow: allow_flag.map(|_| vec!["x".into()]).unwrap_or_default(),
                extra_args: vec![],
                token: None,
            };
            let unit = render_daemon_unit(&s, &d, &Layout::default());
            assert!(!unit.contains("--token-file"), "no token configured: {unit}");
            assert!(help.contains("--token-file"), "comp-{name} has no --token-file\n{help}");
            let exec = unit.lines().find(|l| l.starts_with("ExecStart=")).expect("an ExecStart line");
            for flag in exec.split_whitespace().filter(|w| w.starts_with("--")) {
                assert!(help.contains(flag), "comp-{name} has no {flag}\n--- help ---\n{help}");
            }
            checked += 1;
        }
        if checked == 0 {
            eprintln!("skipping entirely: no comp-<daemon> binaries built under ../reconciler/target/release");
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
        // It shells out to plain binaries, not a wasmtime host.
        assert!(out.contains("\nMemoryDenyWriteExecute=yes\n"), "{out}");
        // A minimal spec asks for neither renewal nor enforcement.
        assert!(!out.contains("--email"), "{out}");
        assert!(!out.contains("--password-file"), "{out}");
        assert!(!out.contains("--enforce-deepseek-offpeak"), "{out}");
        // Unlike a daemon's or the relay's unit, this one is not bound to any
        // app's lifecycle — it outlives all of them.
        assert!(!out.contains("BindsTo"), "{out}");

        for line in out.lines() {
            if line.starts_with('#') || line.starts_with('[') || line.trim().is_empty() {
                continue;
            }
            assert!(!line.contains(" #"), "inline comment would become part of the value: {line:?}");
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
        // comp-goald's own semantics: either alone silently becomes
        // `login: None`, and the daemon dies on the first session renewal
        // instead of refusing to start — catch it here instead.
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
        // Unlike the systemd unit: a LaunchAgent already runs at its owner's
        // own uid, so there is no privilege to drop and nothing for
        // `LoadCredential` to mediate.
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

    /// Same reasoning as `every_flag_we_emit_exists_on_comp_host`: a renamed
    /// `comp-goald` flag should fail a test, not a unit systemd refuses to
    /// start on a real box. Skipped when the binary has not been built.
    #[test]
    fn every_flag_we_emit_exists_on_comp_goald() {
        let bin = std::path::Path::new("../reconciler/target/release/comp-goald");
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
        // Everything before the bare `--` is comp-goald's own flags; what
        // follows is comp-goalrun's, which comp-goald never inspects and
        // this test has no business checking against comp-goald's --help.
        let own = exec.split(" -- ").next().unwrap();
        for flag in own.split_whitespace().filter(|w| w.starts_with("--")) {
            assert!(help.contains(flag), "comp-goald has no {flag}\n--- help ---\n{help}");
        }
    }

    #[test]
    fn the_spec_carries_the_kubernetes_fields_it_does_not_use_yet() {
        // Tier 3 reads `components` + `strategy`. They are optional here so that
        // moving an app up a tier is an edit, not a rewrite (docs/SELFHOST.md).
        let s = spec(
            "name = \"gate\"\ndomain = \"g.example.com\"\nartifact = \"a.wasm\"\n\
             components = [\"gate-domain\", \"record-store\"]\nstrategy = \"fused\"\n",
        );
        assert_eq!(s.components.len(), 2);
        assert_eq!(s.strategy.as_deref(), Some("fused"));
    }
}
