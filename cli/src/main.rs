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

mod fleet;
mod platform;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
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
    /// (`just compose-<app>` produces these).
    pub artifact: String,

    /// Loopback port. Derived from the name when absent, so a spec need not carry
    /// bookkeeping — but an explicit one always wins, and `validate` refuses
    /// duplicates either way.
    #[serde(default)]
    pub port: Option<u16>,
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
    Ok(())
}

/// The `CFG_*` environment file. `wasi:config` key `grace-period-secs` is read by
/// `comp-host` from `CFG_GRACE_PERIOD_SECS`, so the translation happens here, once.
pub fn render_env(spec: &Spec) -> String {
    let mut s = String::new();
    s.push_str("# Generated by selfhost — do not edit; edit the app spec and re-deploy.\n");
    s.push_str("# `wasi:config` keys, as comp-host expects them.\n");
    for (k, v) in &spec.config {
        let name = k.to_uppercase().replace('-', "_");
        // systemd reads this file literally: no shell, so no expansion and no
        // quoting rules to get wrong. A newline would end the assignment, so it is
        // the one character that has to go.
        let clean = v.replace('\n', " ");
        s.push_str(&format!("CFG_{name}={clean}\n"));
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

    let mut s = String::new();
    s.push_str("# Generated by selfhost — do not edit; edit the app spec and re-deploy.\n");
    s.push_str(&format!("[Unit]\nDescription=comp-host: {app} ({})\n", spec.domain));
    s.push_str("After=network-online.target\nWants=network-online.target\n\n");
    s.push_str("[Service]\nType=simple\n");
    s.push_str(&format!("ExecStart={} {}\n", layout.bin.display(), args));
    s.push_str(&format!("EnvironmentFile={}/{}.env\n", layout.env_file.display(), app));
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
             tailscale serve --bg --https=443 --set-path /{name} http://127.0.0.1:{port}\n\
             echo \"https://$(tailscale status --json | \\\n\
               python3 -c 'import json,sys;print(json.load(sys.stdin)[\"Self\"][\"DNSName\"].rstrip(\".\"))')/{name}\"\n",
            name = spec.name,
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

// ---- cli --------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "holon",
    version,
    about = "The Holon platform: components, apps, and the nodes they run on"
)]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Sign in to a platform and remember the session.
    Login {
        #[arg(long, env = "COMP_URL", default_value = "http://127.0.0.1:8080")]
        url: String,
        #[arg(long)]
        email: String,
        #[arg(long)]
        password: String,
        /// Create the account first.
        #[arg(long)]
        register: bool,
    },
    /// Show who the stored session belongs to.
    Whoami,
    /// Components: upload them, list them.
    #[command(subcommand)]
    Component(ComponentCmd),
    /// Applications: a graph of components, deployed onto the lattice.
    #[command(subcommand)]
    App(AppCmd),
    /// Nodes: render the files a bare-metal box needs to run one.
    #[command(subcommand)]
    Node(NodeCmd),
    /// Fleets: the lattice lane — nodes, a reconciler and an ingress across boxes.
    #[command(subcommand)]
    Fleet(FleetCmd),
    /// Organisations: who owns a deployment, when a person belongs to several.
    #[command(subcommand)]
    Org(OrgCmd),
    /// Secrets: values a manifest must never carry, stored by reference.
    #[command(subcommand)]
    Secret(SecretCmd),
    /// Projects: a repository, its credentials, and a queue of goals.
    #[command(subcommand)]
    Project(ProjectCmd),
    /// Goals: the worklist. Nothing starts one but you (ADR-0082).
    #[command(subcommand)]
    Goal(GoalCmd),
}

#[derive(Subcommand)]
enum ComponentCmd {
    /// Upload a .wasm. Reflection is the validation (ADR-0006).
    Push {
        file: PathBuf,
        /// Defaults to the filename, minus `.composed`.
        #[arg(long)]
        id: Option<String>,
    },
    /// What this tenant can use.
    Ls,
}

#[derive(Subcommand)]
enum AppCmd {
    /// Define an app: components, and the links between them.
    Create {
        name: String,
        #[arg(long, default_value = "linked")]
        strategy: String,
        /// Component ids, repeatable.
        #[arg(long = "component", required = true)]
        components: Vec<String>,
        /// `plug:socket:iface`, repeatable.
        #[arg(long = "link")]
        links: Vec<String>,
        /// Which organisation owns it. Defaults to your own.
        #[arg(long)]
        org: Option<String>,
    },
    /// Validate, build the manifest, and store it as a revision. The reconciler
    /// places it on its next pass.
    Deploy {
        id: String,
    },
    Ls,
    Show {
        id: String,
    },
    /// The desired state a revision stores.
    Manifest {
        id: String,
    },
    /// Delete an app. The confirmation is the platform's rule, not this tool's.
    Rm {
        id: String,
        #[arg(long)]
        confirm: String,
    },
}

#[derive(Subcommand)]
enum OrgCmd {
    /// Create one. You become its owner.
    Create {
        name: String,
    },
    /// Every org you belong to, and your role in each.
    Ls,
    /// Mint a single-use join code.
    Invite {
        org: String,
        #[arg(long, default_value = "member")]
        role: String,
    },
    /// Redeem a code.
    Join {
        code: String,
    },
    Members {
        org: String,
    },
    /// Remove someone. Yourself needs no permission; anyone else needs owner.
    Remove {
        org: String,
        subject: String,
    },
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// Add one. ONE repository per project — multi-repo is an open goal, not a
    /// missing feature (ADR-0082).
    Add {
        name: String,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "main")]
        base: String,
        #[arg(long)]
        org: Option<String>,
    },
    /// Every project, with how much work is queued, running and dead-lettered.
    Ls {
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
enum GoalCmd {
    /// Queue one. It sits there until you start it.
    Add {
        project: String,
        title: String,
        /// A path in the repo — `.comp/goals/x.md`. The spec belongs in git,
        /// where it is versioned and content-addressed for free.
        #[arg(long)]
        spec: Option<String>,
        /// Lower runs sooner. An ordering hint for a person reading a worklist.
        #[arg(long)]
        priority: Option<i64>,
    },
    /// The worklist, priority first.
    Ls {
        project: String,
        /// queued | running | awaiting-human | done | failed | abandoned
        #[arg(long)]
        state: Option<String>,
    },
    /// Start one. The only transition a person MUST make for work to happen.
    Start { id: String },
    /// Run a goal to a pull request, here and now.
    ///
    /// Drives a real search — real model, real gate, real forge — over a local
    /// checkout and opens a PR for the winner. This is the whole loop; it just
    /// still takes a person to type it (ADR-0082). Wraps the `comp-goalrun`
    /// binary, which holds the fleet machinery a thin CLI cannot.
    Run {
        /// A local checkout of the target repo, holding `.comp/goal.toml`.
        #[arg(long)]
        checkout: PathBuf,
        /// `owner/name` of the repository the PR opens on.
        #[arg(long)]
        repo: String,
        /// A FILE holding the Anthropic key. Never a value — a path.
        #[arg(long)]
        anthropic_key: PathBuf,
        /// A FILE holding the GitHub token.
        #[arg(long)]
        github_token: PathBuf,
        #[arg(long, default_value_t = 4)]
        branches: u16,
        #[arg(long, default_value_t = 1)]
        rounds: u16,
        #[arg(long, default_value = "claude-haiku-4-5-20251001")]
        model: String,
        #[arg(long, default_value_t = 2)]
        attempts: u32,
        /// Search and rank, but open no PR.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        /// Bring the fleet up and check it serves, without any model call.
        #[arg(long, default_value_t = false)]
        smoke: bool,
    },
    /// Send one to the dead-letter queue, with a reason. Terminal: a retry is a
    /// new goal, so what was tried stays visible.
    Fail {
        id: String,
        #[arg(long)]
        reason: String,
    },
    /// Drop one that was never started.
    Rm { id: String },
}

#[derive(Subcommand)]
enum SecretCmd {
    /// Store one. The VALUE never comes from the command line — an argument
    /// lands in shell history and in `ps` for every other user on the box, and
    /// neither can be taken back.
    ///
    ///   comp secret set openai            # prompts, hidden, asks twice
    ///   comp secret set openai --from ./key.txt
    ///   pbpaste | comp secret set openai  # a pipe stays silent, for scripts
    Set {
        /// The name the reference is built from: `vault://<org>/<name>`.
        name: String,
        /// Read the value from this file instead of stdin.
        #[arg(long)]
        from: Option<PathBuf>,
        /// Which org owns it. Defaults to your personal one.
        #[arg(long)]
        org: Option<String>,
    },
    /// Names and references. There is no command that prints a value, because
    /// there is no endpoint that returns one.
    Ls {
        #[arg(long)]
        org: Option<String>,
    },
    /// Delete one. Anything granted it stops starting on the next reconcile.
    Rm {
        name: String,
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
enum NodeCmd {
    /// Render a TLS front for `comp-ingress`, so the lattice has one HTTPS door.
    ///
    /// TLS is NOT terminated by the ingress. Caddy already does ACME, HTTP-01 and
    /// certificate renewal correctly; reimplementing that inside a reverse proxy
    /// whose whole job is to forward would be work with a known-worse outcome. The
    /// ingress speaks plain HTTP behind it, on loopback.
    Ingress {
        /// The hostname clients use. Every app's `Host` header must resolve here.
        domain: String,
        /// Where `comp-ingress` listens.
        #[arg(long, default_value = "127.0.0.1:8088")]
        upstream: String,
        /// Tailnet-only: bind the Tailscale address and use Caddy's internal CA
        /// rather than ACME, so nothing is exposed publicly and there is still a
        /// secure context (which passkeys and service workers require).
        #[arg(long)]
        tailnet: bool,
    },
    /// Write the unit, env file and route for one app on a self-hosted box.
    Render {
        spec: PathBuf,
        #[arg(long, default_value = "target/selfhost")]
        out: PathBuf,
        #[arg(long, value_enum, default_value_t = Router::Caddy)]
        router: Router,
    },
    /// Check every spec, and refuse the collisions a single spec cannot see:
    /// two apps on one port, one domain, or one name.
    Validate { specs: Vec<PathBuf> },
    /// Print one app's resolved port — what the deploy recipe uses.
    Port { spec: PathBuf },
}

#[derive(Subcommand)]
enum FleetCmd {
    /// Write the units and env file every box in a lattice needs.
    ///
    /// One directory per box, so a deploy is `scp` of a directory rather than a
    /// list of paths to remember. Read it before you trust it to a server.
    Render {
        spec: PathBuf,
        #[arg(long, default_value = "target/fleet")]
        out: PathBuf,
    },
    /// Check a fleet spec: names, addresses, and a lease that outlives a pass.
    Validate { spec: PathBuf },
}

fn load_fleet(path: &Path) -> Result<fleet::Fleet> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let f: fleet::Fleet =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    fleet::check(&f).with_context(|| format!("in {}", path.display()))?;
    Ok(f)
}

fn load(path: &Path) -> Result<Spec> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let spec: Spec =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    check(&spec).with_context(|| format!("in {}", path.display()))?;
    Ok(spec)
}

fn main() -> Result<()> {
    match Args::parse().cmd {
        Cmd::Login { url, email, password, register } => {
            if register {
                platform::register(&url, &email, &password)?;
            } else {
                platform::login(&url, &email, &password)?;
            }
        }
        Cmd::Whoami => platform::whoami()?,
        Cmd::Component(ComponentCmd::Push { file, id }) => platform::component_push(&file, id)?,
        Cmd::Component(ComponentCmd::Ls) => platform::component_ls()?,
        Cmd::App(AppCmd::Create { name, strategy, components, links, org }) => {
            platform::app_create(&name, &strategy, &components, &links, org.as_deref())?
        }
        Cmd::Project(ProjectCmd::Add { name, repo, base, org }) => {
            platform::project_add(&name, &repo, &base, org.as_deref())?
        }
        Cmd::Project(ProjectCmd::Ls { org }) => platform::project_ls(org.as_deref())?,
        Cmd::Goal(GoalCmd::Add { project, title, spec, priority }) => {
            platform::goal_add(&project, &title, spec.as_deref(), priority)?
        }
        Cmd::Goal(GoalCmd::Ls { project, state }) => platform::goal_ls(&project, state.as_deref())?,
        Cmd::Goal(GoalCmd::Start { id }) => platform::goal_start(&id)?,
        Cmd::Goal(GoalCmd::Run {
            checkout,
            repo,
            anthropic_key,
            github_token,
            branches,
            rounds,
            model,
            attempts,
            dry_run,
            smoke,
        }) => {
            // Exec the sibling binary that holds the fleet. Found on PATH, or via
            // COMP_GOALRUN_BIN, or next to this executable — so a `cargo install`
            // and a `just`-built tree both work.
            let bin = std::env::var("COMP_GOALRUN_BIN").unwrap_or_else(|_| "comp-goalrun".into());
            let mut cmd = std::process::Command::new(&bin);
            cmd.arg("--checkout")
                .arg(&checkout)
                .args(["--repo", &repo])
                .arg("--anthropic-key")
                .arg(&anthropic_key)
                .arg("--github-token")
                .arg(&github_token)
                .args(["--branches", &branches.to_string()])
                .args(["--rounds", &rounds.to_string()])
                .args(["--model", &model])
                .args(["--attempts", &attempts.to_string()]);
            if dry_run {
                cmd.arg("--dry-run");
            }
            if smoke {
                cmd.arg("--smoke");
            }
            let status = cmd.status().map_err(|e| {
                anyhow::anyhow!(
                    "could not run `{bin}` ({e}). Build it with `just goal-run` (which builds \
                     and runs in one step), or set COMP_GOALRUN_BIN to its path."
                )
            })?;
            if !status.success() {
                std::process::exit(status.code().unwrap_or(1));
            }
        }
        Cmd::Goal(GoalCmd::Fail { id, reason }) => platform::goal_fail(&id, &reason)?,
        Cmd::Goal(GoalCmd::Rm { id }) => platform::goal_abandon(&id)?,
        Cmd::Secret(SecretCmd::Set { name, from, org }) => {
            platform::secret_set(&name, from.as_ref(), org.as_deref())?
        }
        Cmd::Secret(SecretCmd::Ls { org }) => platform::secret_ls(org.as_deref())?,
        Cmd::Secret(SecretCmd::Rm { name, org }) => platform::secret_rm(&name, org.as_deref())?,
        Cmd::Org(OrgCmd::Create { name }) => platform::org_create(&name)?,
        Cmd::Org(OrgCmd::Ls) => platform::org_ls()?,
        Cmd::Org(OrgCmd::Invite { org, role }) => platform::org_invite(&org, &role)?,
        Cmd::Org(OrgCmd::Join { code }) => platform::org_join(&code)?,
        Cmd::Org(OrgCmd::Members { org }) => platform::org_members(&org)?,
        Cmd::Org(OrgCmd::Remove { org, subject }) => platform::org_remove(&org, &subject)?,
        Cmd::App(AppCmd::Deploy { id }) => platform::app_deploy(&id)?,
        Cmd::App(AppCmd::Ls) => platform::app_ls()?,
        Cmd::App(AppCmd::Show { id }) => platform::app_show(&id)?,
        Cmd::App(AppCmd::Manifest { id }) => platform::app_manifest(&id)?,
        Cmd::App(AppCmd::Rm { id, confirm }) => platform::app_rm(&id, &confirm)?,
        Cmd::Node(NodeCmd::Render { spec, out, router }) => {
            let spec = load(&spec)?;
            let layout = Layout::default();
            let dir = out.join(&spec.name);
            std::fs::create_dir_all(&dir)?;
            let unit = dir.join(format!("comp-{}.service", spec.name));
            let env = dir.join(format!("{}.env", spec.name));
            let route = dir.join(match router {
                Router::Caddy => format!("{}.caddy", spec.name),
                Router::Traefik => format!("{}.yml", spec.name),
                Router::TailscaleServe => format!("{}.serve.sh", spec.name),
            });
            std::fs::write(&unit, render_unit(&spec, &layout))?;
            std::fs::write(&env, render_env(&spec))?;
            std::fs::write(&route, render_route(&spec, router))?;
            println!("{}", dir.display());
            eprintln!(
                "selfhost: {} [{}] -> {} on 127.0.0.1:{} (artifact {})",
                spec.name,
                spec.access,
                spec.domain,
                port_of(&spec),
                spec.artifact
            );
        }
        Cmd::Node(NodeCmd::Validate { specs }) => {
            let mut ports: BTreeMap<u16, String> = BTreeMap::new();
            let mut domains: BTreeMap<String, String> = BTreeMap::new();
            let mut names: BTreeMap<String, String> = BTreeMap::new();
            for path in &specs {
                let spec = load(path)?;
                let where_ = path.display().to_string();
                let port = port_of(&spec);
                if let Some(other) = ports.insert(port, where_.clone()) {
                    bail!("port {port} is claimed by both {other} and {where_} — set `port` explicitly in one");
                }
                if let Some(other) = domains.insert(spec.domain.clone(), where_.clone()) {
                    bail!("domain {} is claimed by both {other} and {where_}", spec.domain);
                }
                if let Some(other) = names.insert(spec.name.clone(), where_.clone()) {
                    bail!("name {} is used by both {other} and {where_}", spec.name);
                }
            }
            println!("{} spec(s) ok, no port/domain/name collisions", specs.len());
        }
        Cmd::Node(NodeCmd::Port { spec }) => println!("{}", port_of(&load(&spec)?)),
        Cmd::Fleet(FleetCmd::Render { spec, out }) => {
            let f = load_fleet(&spec)?;
            let layout = fleet::FleetLayout::default();

            // One directory per BOX, not per unit: a box is what gets scp'd to, and
            // a reconciler standby sharing a box with a node is normal.
            for n in &f.nodes {
                let dir = out.join(&n.name);
                std::fs::create_dir_all(&dir)?;
                std::fs::write(
                    dir.join(format!("comp-node-{}.service", n.name)),
                    fleet::render_node_unit(&f, n, &layout),
                )?;
                println!("{}", dir.display());
            }
            for r in &f.reconcilers {
                let dir = out.join(&r.host);
                std::fs::create_dir_all(&dir)?;
                std::fs::write(
                    dir.join("comp-reconciler.service"),
                    fleet::render_reconciler_unit(&f, r, &layout),
                )?;
                // Rendered empty and installed 0600. A secret that lives in a file
                // you commit is not a secret (ADR-0010).
                std::fs::write(dir.join("reconciler.env"), fleet::render_reconciler_env())?;
                println!("{}", dir.display());
            }
            let dir = out.join(&f.ingress.host);
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("comp-ingress.service"), fleet::render_ingress_unit(&f, &layout))?;
            println!("{}", dir.display());

            eprintln!(
                "fleet: lattice {} — {} node(s), {} reconciler(s), ingress on {} at {}",
                f.lattice,
                f.nodes.len(),
                f.reconcilers.len(),
                f.ingress.host,
                f.ingress.addr
            );
            if f.reconcilers.is_empty() {
                eprintln!(
                    "  note: no reconciler — nothing will converge this lattice. Add [[reconcilers]]."
                );
            } else if f.reconcilers.len() == 1 {
                eprintln!(
                    "  note: one reconciler, so nothing takes over if it dies. A second is a standby, not a conflict (ADR-0072)."
                );
            }
        }
        Cmd::Fleet(FleetCmd::Validate { spec }) => {
            let f = load_fleet(&spec)?;
            println!(
                "fleet ok: {} node(s), {} reconciler(s), ingress on {}",
                f.nodes.len(),
                f.reconcilers.len(),
                f.ingress.host
            );
        }
        Cmd::Node(NodeCmd::Ingress { domain, upstream, tailnet }) => {
            print!("{}", render_ingress_route(&domain, &upstream, tailnet))
        }
    }
    Ok(())
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
        assert!(out.contains("EnvironmentFile=/etc/comp/gate.env"));
        assert!(out.contains("Restart=always"));
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

    #[test]
    fn config_keys_become_the_env_names_comp_host_reads() {
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
        // comp-host maps CFG_GRACE_PERIOD_SECS -> the `grace-period-secs` config key.
        assert!(out.contains("CFG_GRACE_PERIOD_SECS=5"), "{out}");
        assert!(out.contains("CFG_ROUTES=upstream=http://127.0.0.1:9000"), "{out}");
    }

    #[test]
    fn a_newline_in_a_value_cannot_forge_another_variable() {
        let s = spec(
            "name = \"gate\"\ndomain = \"g.example.com\"\nartifact = \"a.wasm\"\n\
             [config]\nk = \"one\\ntwo=three\"\n",
        );
        let out = render_env(&s);
        assert_eq!(out.lines().filter(|l| l.starts_with("CFG_")).count(), 1, "{out}");
        assert!(out.contains("CFG_K=one two=three"), "{out}");
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
