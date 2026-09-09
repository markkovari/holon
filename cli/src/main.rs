//! `holon` — the CLI: renders app specs and fleet layouts into what a box, a
//! lattice node, or wasmCloud actually needs, and a handful of platform/goal/org
//! commands over the control-plane API.
//!
//! The renderers themselves (`Spec`, `check`, every `render_*`) live in
//! [`selfhost`] — pure functions, tested there. This file is the CLI shell around
//! them: argument parsing (`Args`/`Cmd`) and the dispatch in `main`.

mod fleet;
mod platform;
mod selfhost;
mod wadm;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use selfhost::{
    check, port_of, render_daemon_unit, render_env, render_ingress_route, render_relay_unit,
    render_route, render_unit, write_secret_file, Layout, Router, Spec,
};

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
    /// wasmCloud: render an app as a wadm manifest, fused or linked, v1 or v2.
    #[command(subcommand)]
    Wadm(WadmCmd),
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
enum WadmCmd {
    /// Render the wadm Application for one app.
    ///
    /// Topology and API version are flags rather than spec fields on purpose: which
    /// wasmCloud a cluster runs, and whether a graph is small enough to fuse, are
    /// facts about the DEPLOYMENT. The same app spec renders all four.
    Render {
        spec: PathBuf,
        #[arg(long, value_enum, default_value_t = wadm::Topology::Fused)]
        topology: wadm::Topology,
        #[arg(long, value_enum, default_value_t = wadm::ApiVersion::V1)]
        api: wadm::ApiVersion,
        /// Registry the host pulls artifacts from.
        #[arg(long, default_value = "registry.wasmcloud.svc.cluster.local:5000")]
        registry: String,
        /// In-cluster NATS, for the keyvalue provider.
        #[arg(long, default_value = "nats://nats.wasmcloud.svc.cluster.local:4222")]
        nats: String,
        /// Where the http-server provider listens.
        #[arg(long, default_value = "0.0.0.0:8080")]
        addr: String,
        #[arg(long, default_value_t = 1)]
        replicas: u32,
        /// Kubernetes namespace for `--api v2`. Ignored for v1, whose manifest goes
        /// to wadm over NATS rather than to a cluster.
        #[arg(long, default_value = "wasmcloud-v2")]
        namespace: String,
        /// The capability graph, from `comp-capgraph --format json`.
        ///
        /// Required for `--topology linked`: a wadm link carries the WIT namespace,
        /// package and interfaces that say WHICH import it satisfies, and those are
        /// derived from the built artifacts rather than typed out.
        #[arg(long)]
        graph: Option<PathBuf>,
        /// Write here instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Render the operator's WasmCloudHostConfig — the Kubernetes lane's one extra file.
    Host {
        #[arg(long, default_value = "holon")]
        namespace: String,
        #[arg(long, default_value = "holon")]
        lattice: String,
        /// The wasmCloud host version to run. Match the cluster you are deploying to.
        #[arg(long, default_value = "1.6.0")]
        version: String,
        #[arg(long, default_value = "registry.wasmcloud.svc.cluster.local:5000")]
        registry: String,
        #[arg(long, default_value = "nats://nats.wasmcloud.svc.cluster.local:4222")]
        nats: String,
    },
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
            write_secret_file(&env, render_env(&spec).as_bytes())?;
            std::fs::write(&route, render_route(&spec, router))?;
            // Only when the spec asks for one. An app with no timers and no topics
            // must not gain a process that pokes it every ten seconds forever.
            if let Some(t) = &spec.triggers {
                std::fs::write(
                    dir.join(format!("comp-{}-relay.service", spec.name)),
                    render_relay_unit(&spec, t, &layout),
                )?;
            }
            // One unit per daemon this app declared — see `Daemon`'s own doc for
            // why not one shared runner for all twelve.
            for d in &spec.daemons {
                std::fs::write(
                    dir.join(format!("comp-{}.service", d.name)),
                    render_daemon_unit(&spec, d, &layout),
                )?;
                if let Some(token) = &d.token {
                    write_secret_file(
                        &dir.join(format!("{}-{}.token", spec.name, d.name)),
                        token.as_bytes(),
                    )?;
                }
            }
            println!("{}", dir.display());
            eprintln!(
                "selfhost: {} [{}] -> {} on 127.0.0.1:{} (artifact {})",
                spec.name,
                spec.access,
                spec.domain,
                port_of(&spec),
                spec.artifact
            );
            if let Some(t) = &spec.triggers {
                eprintln!("  + relay: POST {} every {}s", t.pump, t.interval);
            }
            for d in &spec.daemons {
                eprintln!("  + daemon: comp-{} on {}", d.name, d.addr);
            }
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
        Cmd::Wadm(WadmCmd::Render {
            spec,
            topology,
            api,
            registry,
            nats,
            addr,
            replicas,
            namespace,
            graph,
            out,
        }) => {
            let s = load(&spec)?;
            let t = wadm::Target { registry, nats, addr, replicas };
            // Refused HERE rather than as a start-time trap on the cluster.
            wadm::check_fusable(&s, topology)?;
            let g = graph.as_deref().map(wadm::Graph::read).transpose()?;
            let y = match api {
                // v2 is a different KIND of document, not a different envelope:
                // wasmCloud 2.x dropped wadm, so this goes to the Kubernetes API.
                wadm::ApiVersion::V2 => {
                    // Read from the artifact, not the graph: `comp:` imports are
                    // host imports and the capability graph does not record them, so
                    // asking it would always answer "fine". Skipped when the artifact
                    // has not been composed yet — that is `compose-<app>`'s error to
                    // give, not this one's.
                    let art = std::path::Path::new(&s.artifact);
                    let mut imports_config = false;
                    if art.exists() {
                        let blocked = wadm::unsupported_on_v2(art)?;
                        if !blocked.is_empty() {
                            bail!(
                                "`{}` imports {} — a wasmCloud 2.x release host provides standard \
                                 WASI and wasmcloud:messaging only, and anything else needs a host \
                                 component plugin, which release images are not built with. \
                                 Deploy this one to tier 1, the lattice, or --api v1.",
                                s.name,
                                blocked.join(", ")
                            );
                        }
                        imports_config = wadm::imports_wasi_config(art)?;
                    } else {
                        eprintln!(
                            "  note: {} is not composed, so its imports were not checked against \
                             what a 2.x host provides",
                            s.artifact
                        );
                    }
                    wadm::render_workload(&s, &namespace, &t, g.as_ref(), imports_config)?
                }
                wadm::ApiVersion::V1 => wadm::render(&s, topology, api, &t, g.as_ref())?,
            };
            match out {
                Some(p) => {
                    if let Some(d) = p.parent() {
                        std::fs::create_dir_all(d)?;
                    }
                    std::fs::write(&p, &y)?;
                    println!("{}", p.display());
                }
                None => print!("{y}"),
            }
            eprintln!("wadm: {} [{:?}/{:?}]", s.name, topology, api);
            if topology == wadm::Topology::Linked {
                let fused = wadm::fusable(&s.components);
                if !fused.is_empty() {
                    // The hybrid gen-manifest.py reached with LATTICE=1, derived
                    // rather than typed: pure compute costs nothing to fuse and saves
                    // a hop each — 1.2ms apiece on v1.
                    eprintln!("  fused in (pure compute, no hop): {}", fused.join(" "));
                }
            }
        }
        Cmd::Wadm(WadmCmd::Host { namespace, lattice, version, registry, nats }) => {
            let t = wadm::Target { registry, nats, ..wadm::Target::default() };
            print!("{}", wadm::render_host_config(&namespace, &lattice, &version, &t));
        }
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

