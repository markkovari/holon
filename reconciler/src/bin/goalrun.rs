//! `comp-goalrun` — take a goal to a pull request, for real.
//!
//! This is the binary behind `holon goal run`: the one command that turns a goal
//! and a repository into an opened PR, with a real model and a real gate. It
//! assembles the pieces that are each already tested in isolation —
//! `generation::search` (the fan-out and the loop), `anthropic-provider` (the
//! model), `checks-runner` + `comp-checks` (the gate), `github-forge` (the PR) —
//! onto one fleet and drives them.
//!
//! ## Why a native binary and not the `comp` CLI
//!
//! `comp` is a thin HTTP client to the control plane. A real run needs the whole
//! substrate up — NATS, hosts, the gate's native runner — which is exactly what
//! `fleet::Fleet` stands up for the tests. So the orchestration lives here, in
//! the crate that owns the fleet, and `holon goal run` shells to it.
//!
//! ## Secrets never touch argv
//!
//! The Anthropic key and the GitHub token arrive as FILE PATHS (`--anthropic-key
//! file`, `--github-token file`); the values are read from those files and handed
//! to the vault as `vault://…=@path`. A path is not a secret; a key on a command
//! line is one in every `ps` and shell history there is.
//!
//! ## What is real and what is not
//!
//! All of it is real. The only thing this does NOT do is pick the goal off a
//! queue — a person still runs the command. That is the last wire, and it is
//! deliberately a person until the interruption rate is understood (ADR-0082).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context, Result};
use clap::Parser;
use comp_reconciler::fleet::{bin_path, free_port, repo_root, Fleet};
use comp_reconciler::compose;
use comp_reconciler::contract::{Answerer, Registry};
use comp_reconciler::generation::{land, Bounds, Entry, Part};
use comp_reconciler::memory::{self, run_id, Memory};
use comp_reconciler::generation as generation_mod;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-goalrun", about = "Run a goal to a pull request, for real.")]
struct Args {
    /// The local checkout of the target repo. Its tracked files are the base tree
    /// the candidates are judged against, and `.comp/goal.toml` is the goal.
    #[arg(long)]
    checkout: PathBuf,
    /// `owner/name` of the repository the PR opens on.
    #[arg(long)]
    repo: String,
    /// The branch to open the PR against.
    #[arg(long, default_value = "main")]
    base: String,
    /// A file holding the Anthropic API key. Read here, never placed in argv.
    #[arg(long)]
    anthropic_key: PathBuf,
    /// A file holding the GitHub token. Read here, never placed in argv.
    #[arg(long)]
    github_token: PathBuf,
    /// A SurrealDB the knowledge pool may use, e.g. `http://127.0.0.1:8000`.
    ///
    /// OPT-IN, and absent by default. Given one, the run deploys the memory app,
    /// asks it whether this goal has already been done, and records every branch's
    /// verdict on the way out. Absent, none of that happens and the loop is
    /// exactly what it was — the database is not part of the platform (ADR-0080),
    /// so a real run must not require one to be up.
    #[arg(long)]
    surreal_url: Option<String>,
    /// The password for that database, as a FILE path. Absent means the server
    /// takes unauthenticated writes, which is a legitimate local setup.
    #[arg(long)]
    surreal_password: Option<PathBuf>,
    /// Skip the whole search when a past passing run of a goal this similar is on
    /// record. Cosine; 0.9 is alpha-swarm2's and is high on purpose — redoing work
    /// costs money, skipping work that was never done is a wrong answer.
    #[arg(long, default_value = "0.9")]
    skip_above: f64,
    /// The model that answers a part's request at a generation boundary.
    ///
    /// A verdict and an interface, not an implementation — so it is the cheap one
    /// by default, and naming it separately is what makes that a decision rather
    /// than an accident (ADR-0086).
    #[arg(long, default_value = "claude-haiku-4-5-20251001")]
    answer_model: String,
    /// Branches per generation.
    #[arg(long, default_value_t = 4)]
    branches: u16,
    /// Generations. 1 for the small first run.
    #[arg(long, default_value_t = 1)]
    rounds: u16,
    /// Repair attempts within a single branch.
    #[arg(long, default_value_t = 2)]
    attempts: u32,
    /// The model. Cheap by default; bump to sonnet/opus for a harder goal.
    #[arg(long, default_value = "claude-haiku-4-5-20251001")]
    model: String,
    /// Per-branch HTTP timeout in seconds. A branch does up to `attempts` model
    /// calls and gate runs, so this is generous.
    #[arg(long, default_value_t = 300)]
    timeout: u64,
    /// Open the PR at the end. Off leaves a dry run: search and rank, propose
    /// nothing — for checking the loop without spending a branch on the forge.
    #[arg(long, default_value_t = false)]
    dry_run: bool,
    /// Bring the fleet up, confirm both apps serve, and exit before any model
    /// call. A $0 check that the deployment, egress and secret grants are right
    /// — run this once before the first real run.
    #[arg(long, default_value_t = false)]
    smoke: bool,
}

/// The goal, as it lives in the repo at `.comp/goal.toml`.
#[derive(Deserialize)]
struct GoalSpec {
    text: String,
    writable: Vec<String>,
    #[serde(default)]
    title: Option<String>,
    /// Ship only tracked files under these path prefixes as the base tree. Empty
    /// means the whole repo, which is right for a small project and impossible
    /// for a large one: the base tree travels over wrpc, and NATS caps a message
    /// near 1 MB, so a 60 MB monorepo cannot ship whole. Scope a goal to the
    /// crate it touches and its path-dependencies, and the subtree fits.
    #[serde(default)]
    base_paths: Vec<String>,
    /// A workspace manifest in the base tree whose `members` list should be
    /// trimmed to `keep_members` before the gate sees it. This is how a single
    /// crate of a shared workspace (one of 130 components) builds standalone: the
    /// gate gets the workspace root with only the target member, so cargo has one
    /// package to build and its `.workspace = true` inheritance still resolves.
    #[serde(default)]
    workspace_manifest: Option<String>,
    #[serde(default)]
    keep_members: Vec<String>,
    /// Name a component crate and the build-scope is DERIVED from the layout —
    /// `base_paths`, `workspace_manifest` and `keep_members` all follow from
    /// `components/<name>/`, so a goal need not hand-list the paths the gate
    /// needs (and get them subtly wrong). An explicitly-set field always wins.
    #[serde(default)]
    component: Option<String>,
    #[serde(rename = "check")]
    checks: Vec<CheckSpec>,
    /// A file in the checkout holding the interface both parts build against.
    ///
    /// Present only for a DECOMPOSED goal, and required by one: two halves that
    /// must compose need something to agree on before either exists, and the
    /// person who described the work is the one who has it (ADR-0086).
    #[serde(default)]
    contract: Option<String>,
    /// The parts. Empty means an ordinary goal — one goal, N competing branches,
    /// one winner — and everything about that path is unchanged.
    ///
    /// With parts, the top-level `[[check]]` list becomes the COMPOSITION gate:
    /// the checks that belong to the whole rather than to either half, and the
    /// ones that can only run over the joined tree.
    #[serde(default, rename = "part")]
    parts: Vec<PartSpec>,
}

/// One half of a decomposed goal.
#[derive(Deserialize)]
struct PartSpec {
    /// What the registry knows it by, and what a request is addressed to.
    name: String,
    text: String,
    /// Disjoint from every other part's, or the merge refuses it: two parts
    /// writing one path is a decomposition bug, not something to resolve.
    writable: Vec<String>,
    /// Files this part is SHOWN but may not write — its held-out tests, most
    /// usefully. Without them a part is told "your tests judge you" and handed no
    /// tests, which is how the first real run of a decomposed goal spent its whole
    /// budget writing blind.
    #[serde(default)]
    context: Vec<String>,
    /// This half's own gate. It runs against the contract alone — the frontend
    /// against fixtures generated from it, the backend against the routes it
    /// promises — so neither part ever waits for the other.
    #[serde(rename = "check")]
    checks: Vec<CheckSpec>,
}

/// The build-scope for a single component crate: the whole crate (src + wit +
/// Cargo.toml) plus the shared workspace root, whose members are trimmed to just
/// this crate. This is the "add the correct path" answer — name the component,
/// and the paths the gate needs come from the layout, not a hand-typed list.
fn component_scope(name: &str) -> (Vec<String>, String, Vec<String>) {
    (
        vec![format!("components/{name}/"), "components/Cargo.toml".to_string()],
        "components/Cargo.toml".to_string(),
        vec![name.to_string()],
    )
}

/// Rewrite a Cargo manifest's `members = [ … ]` to exactly `keep`.
///
/// A flat string edit rather than a toml round-trip, so the rest of the manifest
/// — `[workspace.package]`, `[workspace.dependencies]`, `[profile]`, every comment
/// — survives untouched; only the one array the gate needs narrowed is changed.
fn trim_members(manifest: &str, keep: &[String]) -> String {
    let Some(start) = manifest.find("members") else { return manifest.to_string() };
    let Some(open) = manifest[start..].find('[').map(|i| start + i) else {
        return manifest.to_string();
    };
    let Some(close) = manifest[open..].find(']').map(|i| open + i) else {
        return manifest.to_string();
    };
    let list = keep.iter().map(|m| format!("\"{m}\"")).collect::<Vec<_>>().join(", ");
    format!("{}[{list}]{}", &manifest[..open], &manifest[close + 1..])
}

#[derive(Deserialize)]
struct CheckSpec {
    id: String,
    /// This check is SUPPOSED to be green on the untouched base — a regression
    /// test, a benchmark that must not get slower, an invariant already true.
    ///
    /// Without an escape hatch the gate critic becomes a thing people turn off
    /// rather than a thing they trust, and those are real shapes.
    #[serde(default)]
    may_pass_base: bool,
    #[serde(default = "yes")]
    required: bool,
    #[serde(default = "one")]
    weight: u32,
    command: Vec<String>,
}
fn yes() -> bool {
    true
}
fn one() -> u32 {
    1
}

/// The tracked files the gate materialises and runs its checks over — the
/// source, the tests, and whatever the build needs (`pyproject.toml`, `uv.lock`,
/// `Cargo.toml`). Scoped to `base_paths` when given, so a goal against one crate
/// of a large repo ships that crate and its path-deps, not the whole tree.
fn base_tree(checkout: &Path, base_paths: &[String]) -> Result<Vec<Value>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["ls-files"])
        .output()
        .context("git ls-files")?;
    if !out.status.success() {
        bail!("git ls-files failed in {}", checkout.display());
    }
    let mut tree = Vec::new();
    let mut bytes = 0usize;
    for path in String::from_utf8_lossy(&out.stdout).lines() {
        if !base_paths.is_empty() && !base_paths.iter().any(|p| path.starts_with(p.as_str())) {
            continue;
        }
        let full = checkout.join(path);
        // Skip anything that is not valid UTF-8 (a stray binary) rather than fail
        // the whole run; a source tree is text and a binary in it is not a gate
        // input.
        let Ok(content) = std::fs::read_to_string(&full) else { continue };
        bytes += content.len();
        tree.push(json!({ "path": path, "content": content }));
    }
    if tree.is_empty() {
        bail!(
            "no tracked files under {:?} in {} — check base_paths",
            base_paths,
            checkout.display()
        );
    }
    // The whole tree travels over wrpc as one message. NATS refuses one past ~1
    // MB, and the failure is opaque, so catch it here with an actionable message.
    if bytes > 900_000 {
        bail!(
            "the base tree is {:.1} MB, over the ~1 MB a run can ship — scope the goal with \
             base_paths to the crate it touches (a monorepo cannot ship whole)",
            bytes as f64 / 1_048_576.0
        );
    }
    Ok(tree)
}

fn head_commit(checkout: &Path) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["rev-parse", "HEAD"])
        .output()
        .context("git rev-parse")?;
    if !out.status.success() {
        bail!("git rev-parse HEAD failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The native gate runner, alive for the run.
struct Checks {
    child: Child,
    port: u16,
    _dir: tempfile::TempDir,
}
impl Drop for Checks {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Checks {
    fn start(allow: &[&str], check_env: &[String]) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let port = free_port();
        let mut cmd = Command::new(bin_path("comp-checks"));
        cmd.args(["--addr", &format!("127.0.0.1:{port}")])
            .arg("--work-dir")
            .arg(dir.path())
            .args(["--timeout", "120"]);
        for a in allow {
            cmd.args(["--allow", a]);
        }
        for e in check_env {
            cmd.args(["--check-env", e]);
        }
        let child = cmd.stdout(Stdio::null()).stderr(Stdio::inherit()).spawn()?;
        let me = Self { child, port, _dir: dir };
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return Ok(me);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        bail!("comp-checks never listened on {port}");
    }
}

/// Write a fixture with placeholders substituted, to a temp path.
fn render(fixture: &str, subs: &[(&str, &str)]) -> Result<PathBuf> {
    let mut yaml = std::fs::read_to_string(repo_root().join("fixtures").join(fixture))
        .with_context(|| format!("reading fixture {fixture}"))?;
    for (k, v) in subs {
        yaml = yaml.replace(k, v);
    }
    let out = std::env::temp_dir().join(format!("comp-goalrun-{}-{fixture}", std::process::id()));
    std::fs::write(&out, yaml)?;
    Ok(out)
}

fn artifacts() -> Result<Vec<String>> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let mut out = Vec::new();
    // The memory app's five are here unconditionally: an artifact nothing places
    // costs nothing, and making the list conditional would mean two ways to be
    // missing a file.
    for (id, file) in [
        ("cprobe", "contract_probe.wasm"),
        ("registry", "contract_registry.wasm"),
        ("cgraph", "knowledge_graph.wasm"),
        ("lprobe", "llm_probe.wasm"),
        ("allm", "anthropic_provider.wasm"),
        ("mprobe", "memory_probe.wasm"),
        ("memory", "knowledge_memory.wasm"),
        ("graph", "knowledge_graph.wasm"),
        ("search", "search_index.wasm"),
        ("mllm", "mock_provider.wasm"),
        ("probe", "driver_probe.wasm"),
        ("driver", "agent_driver.wasm"),
        ("writer", "agent_writer.wasm"),
        ("llm", "anthropic_provider.wasm"),
        ("gate", "checks_runner.wasm"),
        ("sprobe", "select_probe.wasm"),
        ("selector", "graph_selector.wasm"),
        ("forge", "github_forge.wasm"),
    ] {
        let p = dir.join(file);
        if !p.exists() {
            bail!("missing {} — run `just build`", p.display());
        }
        out.push(format!("{id}={}", p.display()));
    }
    Ok(out)
}

/// Poll an app's root route until it answers — cheap readiness that never calls
/// the model (the probe's `/` returns a static service line).
fn wait_serving(port: u16, host: &str, within: Duration) -> Result<()> {
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build()?;
    let deadline = Instant::now() + within;
    let mut last = String::new();
    while Instant::now() < deadline {
        match http.get(format!("http://127.0.0.1:{port}/")).header("host", host).send() {
            Ok(r) if r.status().is_success() => return Ok(()),
            Ok(r) => last = format!("HTTP {}", r.status()),
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    bail!("{host} never served within {within:?} — last: {last}");
}

fn main() -> Result<()> {
    let args = Args::parse();

    let goal_path = args.checkout.join(".comp/goal.toml");
    let mut goal: GoalSpec = toml::from_str(
        &std::fs::read_to_string(&goal_path)
            .with_context(|| format!("reading {}", goal_path.display()))?,
    )
    .context("parsing .comp/goal.toml")?;
    if goal.checks.is_empty() {
        bail!("the goal has no checks — an empty gate accepts everything");
    }
    // A named component derives the build-scope from the layout. An explicitly
    // set field always wins, so a goal can name the component and still override
    // one path if its crate is unusual.
    if let Some(name) = goal.component.clone() {
        let (bp, wm, km) = component_scope(&name);
        if goal.base_paths.is_empty() {
            goal.base_paths = bp;
        }
        if goal.workspace_manifest.is_none() {
            goal.workspace_manifest = Some(wm);
        }
        if goal.keep_members.is_empty() {
            goal.keep_members = km;
        }
    }

    // The base tree and the files the agent starts from.
    let mut tree = base_tree(&args.checkout, &goal.base_paths)?;
    // Trim a shared workspace manifest to the goal's target member, so one crate
    // of a big workspace builds standalone in the gate.
    if let (Some(manifest), false) = (&goal.workspace_manifest, goal.keep_members.is_empty()) {
        for e in tree.iter_mut() {
            if e["path"] == serde_json::json!(manifest) {
                let trimmed = trim_members(e["content"].as_str().unwrap_or_default(), &goal.keep_members);
                e["content"] = serde_json::json!(trimmed);
            }
        }
    }
    let base_commit = head_commit(&args.checkout)?;
    let context: Vec<Value> = goal
        .writable
        .iter()
        .filter_map(|w| {
            std::fs::read_to_string(args.checkout.join(w))
                .ok()
                .map(|c| json!({ "path": w, "content": c }))
        })
        .collect();

    let checks: Vec<Value> = goal
        .checks
        .iter()
        .map(|c| json!({ "id": c.id, "required": c.required, "weight": c.weight, "command": c.command }))
        .collect();

    // The commands the gate is allowed to run: the first word of each check.
    // Deduped, so `--allow uv` appears once however many checks use it.
    //
    // EVERY check, including the parts'. A decomposed goal's top-level list is the
    // composition gate alone, so deriving the allow-list from it left every part's
    // own command refused by the runner — a whole run scoring zero for a reason
    // that had nothing to do with the code.
    let mut allow: Vec<&str> = goal
        .checks
        .iter()
        .chain(goal.parts.iter().flat_map(|p| p.checks.iter()))
        .filter_map(|c| c.command.first().map(String::as_str))
        .collect();
    allow.sort_unstable();
    allow.dedup();

    println!("goal: {}", goal.text.lines().next().unwrap_or_default());
    println!(
        "repo: {}  base: {}  branches: {}  rounds: {}  model: {}",
        args.repo, args.base, args.branches, args.rounds, args.model
    );
    println!("gate allows: {allow:?}");

    // A warm, SHARED tool cache for the gate. Without it comp-checks gives each
    // candidate a fresh HOME, so `uv` re-downloads its toolchain from a cold
    // cache every time and the run times out. These dirs persist between runs, so
    // the cost is paid once, ever.
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let uv_cache = format!("{home}/.cache/comp-goalrun/uv");
    let uv_python = format!("{home}/.cache/comp-goalrun/uv-python");
    // A shared, persistent cargo cache. The registry (CARGO_HOME) is downloaded
    // once ever; the target dir keeps compiled dependencies so a candidate only
    // recompiles the crate it changed — seconds, not the cold minutes a fresh
    // HOME would force. This is what makes a cargo gate viable at all.
    let cargo_home = format!("{home}/.cache/comp-goalrun/cargo-home");
    let cargo_target = format!("{home}/.cache/comp-goalrun/cargo-target");
    for d in [&uv_cache, &uv_python, &cargo_home, &cargo_target] {
        std::fs::create_dir_all(d).ok();
    }
    let mut check_env = vec![
        format!("UV_CACHE_DIR={uv_cache}"),
        format!("UV_PYTHON_INSTALL_DIR={uv_python}"),
        format!("CARGO_HOME={cargo_home}"),
        format!("CARGO_TARGET_DIR={cargo_target}"),
        // cargo wants a real registry index and network on a cold cache.
        "CARGO_NET_OFFLINE=false".into(),
    ];
    // `cargo` is usually a rustup shim, and under the gate's cleared environment
    // it cannot choose a toolchain — no RUSTUP_HOME, no default. Pass both, so the
    // shim resolves the same toolchain the pre-warm used. Read from the ambient
    // environment (the operator's), never the agent's.
    let rustup_home =
        std::env::var("RUSTUP_HOME").unwrap_or_else(|_| format!("{home}/.rustup"));
    let toolchain = Command::new("rustup")
        .args(["show", "active-toolchain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.split_whitespace().next().map(String::from))
        .unwrap_or_else(|| "stable".into());
    check_env.push(format!("RUSTUP_HOME={rustup_home}"));
    check_env.push(format!("RUSTUP_TOOLCHAIN={toolchain}"));

    // Pre-warm: run each check once IN THE CHECKOUT with the same caches, so the
    // toolchain download and the dependency compile happen once, outside any
    // request deadline, before a candidate is ever judged. The result does not
    // matter here — only the cache it leaves behind.
    for c in &goal.checks {
        let tool = c.command.first().map(String::as_str);
        if matches!(tool, Some("uv") | Some("cargo")) {
            println!("warming the gate cache ({}) …", c.command.join(" "));
            let _ = Command::new(&c.command[0])
                .args(&c.command[1..])
                .current_dir(&args.checkout)
                .env("UV_CACHE_DIR", &uv_cache)
                .env("UV_PYTHON_INSTALL_DIR", &uv_python)
                .env("CARGO_HOME", &cargo_home)
                .env("CARGO_TARGET_DIR", &cargo_target)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    // Bring the gate up first, so the driver fixture can point at it.
    let gate = Checks::start(&allow, &check_env)?;

    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    // One guest request does a model call plus a test suite; the ingress's 30s
    // default backend timeout kills that as "n1 timed out". Give it room.
    std::env::set_var("COMP_FLEET_BACKEND_TIMEOUT", "240");
    // And the wrpc budget between components: the same nested model+gate call
    // blows the host's 30s default ("data receipt timed out"). Inherited by the
    // hosts the fleet spawns.
    std::env::set_var("COMP_RPC_TIMEOUT_SECS", "240");
    // Trace outbound dials, so a stalled model call shows whether the host got a
    // response back at all.
    std::env::set_var("COMP_TRACE_EGRESS", "1");
    let driver_spec = render(
        "goalrun-driver.yaml",
        &[("CHECKS_PORT", &gate.port.to_string()), ("ANTHROPIC_MODEL", &args.model)],
    )?;
    let forge_spec = render("goalrun-forge.yaml", &[("FORGE_REPO", &args.repo)])?;

    // Secrets by file: only the PATHS reach argv.
    let mut secrets = vec![
        format!("vault://acme/anthropic=@{}", args.anthropic_key.display()),
        format!("vault://acme/forge=@{}", args.github_token.display()),
    ];

    let mut specs =
        vec![driver_spec.to_str().unwrap().to_string(), forge_spec.to_str().unwrap().to_string()];

    // A decomposed goal needs somewhere to keep the contract, and that is a
    // database nothing here deploys. Refused up front rather than half-run.
    if !goal.parts.is_empty() {
        if args.surreal_url.is_none() {
            bail!(
                "this goal has {} part(s), which need a contract registry — pass --surreal-url \
                 (the registry keeps versions and the negotiation history in it)",
                goal.parts.len()
            );
        }
        if goal.contract.is_none() {
            bail!(
                "this goal has parts but no `contract = \"…\"` — two halves that must compose \
                 need something to agree on before either exists"
            );
        }
    }

    // The knowledge pool, only if a database was named.
    if let Some(url) = &args.surreal_url {
        // The graph's egress allow-list is a socket, not a URL — and it is the
        // one address it may dial (ADR-0008).
        let egress = url
            .split("://")
            .nth(1)
            .map(|rest| rest.split('/').next().unwrap_or(rest).to_string())
            .unwrap_or_else(|| url.clone());
        let memory_spec = render(
            "goalrun-memory.yaml",
            &[("SURREAL_URL", url), ("SURREAL_DB", "goalmemory"), ("SURREAL_EGRESS", &egress)],
        )?;
        specs.push(memory_spec.to_str().unwrap().to_string());
        // A database with no auth is a legitimate local setup, so the secret is
        // only granted when a password file was given. The vault reference in the
        // fixture resolves to empty otherwise, which `knowledge-graph` treats as
        // "no password" rather than as a failure.
        if let Some(path) = &args.surreal_password {
            secrets.push(format!("vault://acme/surreal=@{}", path.display()));
        }
        if !goal.parts.is_empty() {
            specs.push(
                render(
                    "goalrun-contract.yaml",
                    &[("SURREAL_URL", url), ("SURREAL_EGRESS", &egress)],
                )?
                .to_str()
                .unwrap()
                .to_string(),
            );
            specs.push(
                render("goalrun-answer.yaml", &[("ANSWER_MODEL", &args.answer_model)])?
                    .to_str()
                    .unwrap()
                    .to_string(),
            );
        }
    }
    let spec_refs: Vec<&str> = specs.iter().map(String::as_str).collect();
    let art = artifacts()?;

    println!("starting fleet …");
    let fleet = Fleet::start_with_secrets("goalrun", &spec_refs, &art, &secrets);
    let port = fleet.ingress_port;

    wait_serving(port, "goalrun.acme.test", Duration::from_secs(180))?;
    wait_serving(port, "goalland.acme.test", Duration::from_secs(180))?;

    // The knowledge pool, if one was deployed. An app that will not serve is
    // reported and then ignored: this is the half of the run that must never stop
    // it (see `memory.rs`).
    let memory = match &args.surreal_url {
        None => None,
        Some(url) => match wait_serving(port, "goalmemory.acme.test", Duration::from_secs(180)) {
            Ok(()) => {
                println!("knowledge pool serving, backed by {url}");
                Some(Memory {
                    url: format!("http://127.0.0.1:{port}"),
                    host: "goalmemory.acme.test".to_string(),
                    timeout: Duration::from_secs(30),
                })
            }
            Err(e) => {
                println!("knowledge pool did not come up ({e}) — running without it");
                None
            }
        },
    };

    // --- criticise the gate, before the money -------------------------------
    //
    // A check that already passes on the base tree cannot judge a candidate: one
    // that changes nothing satisfies it. The first real decomposed run on this
    // repository scored 1000 on two candidates that had deleted their own
    // component exports, because `cargo component check` passes on a crate that
    // implements none of its world (goal 07). This is the cheapest possible place
    // to find that out — before a generation buys the wrong answer.
    let excused: Vec<String> = goal
        .checks
        .iter()
        .chain(goal.parts.iter().flat_map(|p| p.checks.iter()))
        .filter(|c| c.may_pass_base)
        .map(|c| c.id.clone())
        .collect();
    let mut every_check: Vec<Value> = checks.clone();
    for p in &goal.parts {
        every_check.extend(p.checks.iter().map(|c| json!({
            "id": c.id, "required": c.required, "weight": c.weight, "command": c.command,
        })));
    }
    match compose::criticise(
        &format!("http://127.0.0.1:{}/check", gate.port),
        &base_commit,
        &json!(tree),
        &json!(every_check),
        &excused,
        Duration::from_secs(args.timeout),
    ) {
        Ok(vacuous) => {
            for v in vacuous.iter().filter(|v| v.excused) {
                println!("gate: `{}` passes on the base, and says it is meant to", v.id);
            }
            let refusals = compose::refusal(&vacuous);
            if !refusals.is_empty() {
                println!("\nREFUSED — this gate cannot judge anything:\n");
                for r in &refusals {
                    println!("  · {r}");
                }
                println!(
                    "\nNothing was spent. A gate that passes on the code as it stands \n\
                     accepts a candidate that changes nothing."
                );
                return Ok(());
            }
            println!("gate: every check fails on the base tree, so every check can judge");
        }
        // Reported, not fatal: the critic is a guard, and a guard that cannot run
        // must not stop a run a person asked for.
        Err(e) => println!("gate: could not be criticised ({e}) — running anyway"),
    }

    if args.smoke {
        // Both apps serving already proves a lot: an app whose secret cannot be
        // granted, or whose egress is malformed, fails to START and never serves
        // (select.rs saw exactly this). So reaching here means links resolve,
        // egress allow-lists parsed, and BOTH secrets were granted.
        //
        // A max_attempts:0 run is refused by the driver BEFORE any model call, so
        // this last round-trip proves probe→driver without spending anything.
        let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(60)).build()?;
        let probe = http
            .post(format!("http://127.0.0.1:{port}/run"))
            .header("host", "goalrun.acme.test")
            .body(json!({
                "text": goal.text, "writable": goal.writable, "context": context,
                "previous": [], "checks": checks, "base_commit": base_commit,
                "base_tree": tree, "max_attempts": 0, "seed": 1,
            }).to_string())
            .send()?;
        let body: Value = serde_json::from_str(&probe.text().unwrap_or_default()).unwrap_or(Value::Null);
        println!("\nSMOKE OK:");
        println!("  · both graphs started and serve → links, egress and secret GRANTS are correct");
        println!("  · driver reachable → {body}");

        // A decomposed goal brings up two more apps and a database, and every one
        // of them can be proved for FREE: an app whose secret cannot be granted or
        // whose egress is malformed never serves, publishing the contract exercises
        // the registry through the graph to a real SurrealDB, and `describe` asks
        // the provider what it is without asking it to think.
        if !goal.parts.is_empty() {
            wait_serving(port, "goalcontract.acme.test", Duration::from_secs(180))?;
            wait_serving(port, "goalanswer.acme.test", Duration::from_secs(180))?;
            println!("  · the contract registry and the answer door serve");

            let registry = Registry {
                url: format!("http://127.0.0.1:{port}"),
                host: "goalcontract.acme.test".into(),
                timeout: Duration::from_secs(60),
            };
            let contract_path = goal.contract.clone().unwrap_or_default();
            let contract = std::fs::read_to_string(args.checkout.join(&contract_path))
                .with_context(|| format!("reading the contract at {contract_path}"))?;
            match registry.publish(&contract) {
                Ok(v) => println!(
                    "  · contract v{v} published from {contract_path} → registry → graph → \
                     SurrealDB, and the database's secret was granted"
                ),
                // A second smoke run against the same database finds the contract
                // already there, which proves the same chain and is not a failure.
                Err(e) if e.contains("already published") => match registry.current() {
                    Ok(c) => println!(
                        "  · contract v{} already in the registry, and readable → the whole \
                         chain to SurrealDB works",
                        c.number
                    ),
                    Err(e) => bail!("the registry has a contract it cannot read back: {e}"),
                },
                Err(e) => bail!("the contract registry is not usable: {e}"),
            }
            let http = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?;
            let describe = http
                .get(format!("http://127.0.0.1:{port}/describe"))
                .header("host", "goalanswer.acme.test")
                .send()?;
            let d: Value =
                serde_json::from_str(&describe.text().unwrap_or_default()).unwrap_or(Value::Null);
            println!("  · the answering model is reachable and says it is → {d}");
            println!(
                "  · parts: {}",
                goal.parts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
            );
        }

        println!("\nWhat smoke does NOT check (needs a real call, costs money):");
        println!("  · that the Anthropic key VALUE is accepted");
        println!("  · that the GitHub token VALUE can open a PR");
        println!("  · that `{}` actually runs under the gate", allow.join(" "));
        if !goal.parts.is_empty() {
            println!("  · that the parts negotiate — the first request costs one small call");
        }
        println!("\nRun for real by dropping --smoke.");
        return Ok(());
    }
    // --- has this already been done? -------------------------------------
    //
    // ONCE per goal, before anything is spawned. This is the call that saves a
    // whole generation, and it is the only one whose failure mode had to be
    // decided rather than defaulted: an unreachable pool answers "no", because
    // redoing work costs money and skipping work that was never done is a silent
    // wrong answer.
    if let Some(m) = &memory {
        match m.already_done(&goal.text, args.skip_above) {
            Ok(Some(prior)) => {
                println!("\nALREADY DONE — {}", prior.summary());
                println!(
                    "\n  no branches spawned. Lower --skip-above (now {:.2}) or clear the pool \n                       if this is not the same work.",
                    args.skip_above
                );
                return Ok(());
            }
            Ok(None) => println!("nothing on record for this goal; running it"),
            Err(e) => println!("could not ask the knowledge pool ({e}) — doing the work"),
        }
    }

    // --- a DECOMPOSED goal ---------------------------------------------------
    //
    // Parts that compose rather than branches that compete: each half runs its own
    // generations against a shared contract, asks the other for changes it needs,
    // and the winners are merged into one tree judged by the goal's own checks
    // (ADR-0086). One pull request at the end.
    if !goal.parts.is_empty() {
        return decomposed(&args, &goal, port, gate.port, &context, &tree, &base_commit, &checks);
    }

    println!("fleet serving; running the search …\n");

    let plan = json!({
        "text": goal.text,
        "writable": goal.writable,
        "context": context,
        "previous": [],
        "checks": checks,
        "base_commit": base_commit,
        "base_tree": tree,
        "max_attempts": args.attempts,
        "seed": 1,
    });

    // --- what each branch is allowed to read --------------------------------
    //
    // Varied ACROSS branches on purpose: a generation whose branches all read the
    // same top-k is an expensive way to run one branch (ADR-0081's herding), and
    // the branch that reads nothing is the only way to tell whether the pool helps
    // at all. `default_strategies` already keeps one branch from reading the
    // previous winner; that same branch reads no lessons either.
    let mut strategies = generation_mod::default_strategies(args.branches);
    let mut read_by_branch: Vec<Vec<String>> = vec![Vec::new(); strategies.len()];
    if let Some(m) = &memory {
        for (i, s) in strategies.iter_mut().enumerate() {
            if !s.reads_prior {
                continue; // the control arm reads nothing, and that is the point
            }
            let reading = memory::Reading {
                // Deliberately unequal: 3, 4, 5 … so two branches do not arrive at
                // the same prompt by arriving at the same advice.
                k: 3 + (i as u32 % 3),
                budget: 1200,
                pools: match i % 3 {
                    0 => vec![],                                   // everything
                    1 => vec!["errors".into()],                    // only what failed
                    _ => vec!["patterns".into(), "solutions".into()], // only what worked
                },
            };
            match m.recall(&goal.text, &reading) {
                Ok(lessons) if lessons.is_empty() => {}
                Ok(lessons) => {
                    println!(
                        "  branch-{i} reads {} lesson(s) [{}]",
                        lessons.len(),
                        lessons.iter().map(|l| l.ns.as_str()).collect::<Vec<_>>().join(", ")
                    );
                    read_by_branch[i] = lessons.iter().map(|l| l.key.clone()).collect();
                    s.knowledge = memory::render(&lessons);
                }
                // A pool that is down costs a branch its advice and nothing else.
                Err(e) => println!("  branch-{i} runs cold: {e}"),
            }
        }
        let distinct: std::collections::BTreeSet<&Vec<String>> = read_by_branch.iter().collect();
        if strategies.len() > 1 {
            println!(
                "  knowledge: {} distinct reading(s) across {} branches{}",
                distinct.len(),
                strategies.len(),
                if distinct.len() == 1 { " — every branch read the same thing, which is herding" } else { "" }
            );
        }
    }

    let driver_url = format!("http://127.0.0.1:{port}/run");
    let timeout = Duration::from_secs(args.timeout);
    let bounds = Bounds { branches: args.branches, max_rounds: args.rounds, max_tokens: 0, patience: 0 };

    // A fresh seed each run so branches are not a replay of a previous run's.
    let seed = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_secs();
    let found = generation_mod::search_with(
        &driver_url,
        "goalrun.acme.test",
        &plan,
        &strategies,
        bounds,
        seed,
        timeout,
    );

    // Every attempt of every branch, so the run is legible even when it fails.
    let mut entries: Vec<Entry> = Vec::new();
    let mut recorded = 0usize;
    // The accepted branch with the highest score, as (run id, score) — the run the
    // pull request will be attributed to. `search` already picks the winning
    // ENTRY; what is needed here is the run id it was recorded under, which only
    // this walk knows.
    let mut winner: Option<(String, u64)> = None;
    for (r, round) in found.rounds.iter().enumerate() {
        for e in &round.entries {
            // One verdict per BRANCH, not one per generation: the count of failed
            // attempts on a goal is what says whether another generation is worth
            // buying, and a generation-level record cannot say it. The artifact is
            // empty here — nothing has been opened yet — and the landing path
            // re-reports the winner with it, which is free because a verdict edge
            // is keyed by (goal, run).
            let run = run_id(seed, r, &e.branch);
            if e.accepted && winner.as_ref().is_none_or(|(_, best)| e.score > *best) {
                winner = Some((run.clone(), e.score));
            }
            if let Some(m) = &memory {
                match m.evaluated(&goal.text, &run, e.score, e.accepted, "") {
                    Ok(()) => recorded += 1,
                    Err(err) => println!("  (verdict for {run} not recorded: {err})"),
                }
                // What happened to what this branch READ. The only thing that moves
                // a lesson's standing, and the reason retrieval gets better rather
                // than merely existing: a lesson present when runs fail sinks.
                let idx = e.branch.rsplit('-').next().and_then(|n| n.parse::<usize>().ok());
                if let Some(keys) = idx.and_then(|i| read_by_branch.get(i)) {
                    if let Err(err) = m.attribute(keys, &run, e.accepted) {
                        println!("  (what {run} read was not attributed: {err})");
                    }
                }
            }
            println!(
                "  gen {r} {:<9} accepted={:<5} score={:<5} attempts={} tokens={} {}",
                e.branch,
                e.accepted,
                e.score,
                e.attempts,
                e.spent_tokens,
                if e.note.is_empty() { String::new() } else { format!("[{}]", e.note) }
            );
            entries.push(e.clone());
        }
    }
    println!(
        "\nsearch: {:?}, {} tokens across {} branch-runs",
        found.stopped,
        found.spent_tokens,
        entries.len()
    );
    if memory.is_some() {
        println!(
            "knowledge: {recorded}/{} verdicts recorded — a later run asking for this goal \n               will see them",
            entries.len()
        );
    }

    // When a branch never ran (a transport note rather than a verdict), the
    // reason is in the host and ingress logs, which the fleet's tempdir throws
    // away on exit. Surface the tail of each so one failed run is diagnosable.
    // A transport note, OR a branch that produced no candidate at all (0 tokens,
    // not accepted) — the latter is an agent that trapped or errored before it
    // ever called the model, and the reason is only in the host log.
    if entries.iter().any(|e| !e.note.is_empty() || (e.spent_tokens == 0 && !e.accepted)) {
        let tail = |s: &str, n: usize| {
            let lines: Vec<&str> = s.lines().collect();
            lines[lines.len().saturating_sub(n)..].join("\n")
        };
        eprintln!("\n===== host n1 (last 60 lines) =====\n{}", tail(&fleet.node_log("n1"), 60));
        eprintln!("\n===== ingress (last 25 lines) =====\n{}", tail(&fleet.ingress_log(""), 25));
    }

    if !found.accepted {
        let best = found.best.as_ref().map(|e| e.score).unwrap_or(0);
        println!("\nNothing passed the gate (best score {best}). No PR opened.");
        if let Some(b) = &found.best {
            println!("closest failing checks: {}", b.failures);
        }
        return Ok(());
    }

    if args.dry_run {
        let best = found.best.as_ref().unwrap();
        println!("\n[dry run] a candidate passed (score {}); not opening a PR.", best.score);
        return Ok(());
    }

    // Land the winner. A unique branch name per run, because a PR cannot reuse one.
    let title = goal.title.clone().unwrap_or_else(|| goal.text.lines().next().unwrap_or("a candidate").to_string());
    let branch = format!("graph/{}", seed);
    let landing = json!({
        "branch": branch,
        "base": args.base,
        "title": title,
        "body": format!(
            "Automated candidate from a graph-engineering run.\n\n\
             {} branch(es) explored this goal; the winner passed the gate.\n\n\
             Goal:\n\n{}\n",
            entries.len(), goal.text
        ),
        "message": title,
    });

    println!("\nopening a pull request on {} …", args.repo);
    let select_url = format!("http://127.0.0.1:{port}/land");
    match land(&select_url, "goalland.acme.test", &entries, landing, timeout) {
        Ok(v) if v["url"].is_string() => {
            let url = v["url"].as_str().unwrap();
            println!("\n  PR opened: {url}");
            println!("  branch: {}  commit: {}", v["branch"], v["commit"]);
            // Re-report the winning run, now that there is something addressable
            // to point the next run at. Idempotent per (goal, run), so this
            // attaches the pull request without inventing a second evaluation.
            if let (Some(m), Some(w)) = (&memory, &winner) {
                if let Err(e) = m.evaluated(&goal.text, &w.0, w.1, true, url) {
                    println!("  (the pull request was not recorded against the goal: {e})");
                }
            }
        }
        Ok(v) => println!("\n  the forge answered but opened no PR: {v}"),
        Err(e) => println!("\n  landing failed: {e}"),
    }
    Ok(())
}

/// One goal, K parts, one pull request.
///
/// The shape differs from the ordinary path in exactly one way that matters: a
/// generation that produces a brilliant backend and no frontend has produced
/// nothing, so there is no "best" to land until every part is green.
#[allow(clippy::too_many_arguments)]
fn decomposed(
    args: &Args,
    goal: &GoalSpec,
    port: u16,
    checks_port: u16,
    context: &[Value],
    tree: &[Value],
    base_commit: &str,
    composition_checks: &[Value],
) -> Result<()> {
    let registry = Registry {
        url: format!("http://127.0.0.1:{port}"),
        host: "goalcontract.acme.test".into(),
        timeout: Duration::from_secs(60),
    };
    let answerer = Answerer {
        url: format!("http://127.0.0.1:{port}"),
        host: "goalanswer.acme.test".into(),
        timeout: Duration::from_secs(180),
    };
    wait_serving(port, "goalcontract.acme.test", Duration::from_secs(180))?;
    wait_serving(port, "goalanswer.acme.test", Duration::from_secs(180))?;

    // The human's contract, from the file named in the goal spec.
    let contract_path = goal.contract.clone().unwrap_or_default();
    let body = std::fs::read_to_string(args.checkout.join(&contract_path))
        .with_context(|| format!("reading the contract at {contract_path}"))?;
    // `publish` refuses a second contract on purpose: one appearing mid-run would
    // silently move what every part builds against. But a repeat run of the same
    // goal — smoke then real, or a second attempt after a failure — finds its own
    // contract already there, and dying at the door would make `--smoke` something
    // you can only afford to run once.
    //
    // So: continue on what is stored, and refuse only when it DIFFERS from the
    // file. A stored contract that no longer matches the goal is the one case
    // where carrying on would have every part building against a version the
    // person editing the file cannot see.
    let version = match registry.publish(&body) {
        Ok(v) => {
            println!("contract v{v} published from {contract_path}");
            v
        }
        Err(e) if e.contains("already published") => {
            let current = registry
                .current()
                .map_err(|e| anyhow::anyhow!("a contract is registered but unreadable: {e}"))?;
            if current.body.trim() != body.trim() && current.number == 1 {
                bail!(
                    "the registry holds a contract v{} that is not what {contract_path} says. \
                     It was published by an earlier run and amendments are made through \
                     ask/answer, not by editing the file — so either restore the file, or give \
                     this goal a database of its own (`--surreal-url` at a fresh one).",
                    current.number
                );
            }
            println!(
                "contract v{} already registered{}",
                current.number,
                if current.number > 1 { " (amended by an earlier run)" } else { "" }
            );
            current.number
        }
        Err(e) => bail!("publishing the contract: {e}"),
    };
    // What the parts build against is whatever is canonical NOW, which after an
    // earlier run's negotiation may be later than v1.
    let body = registry
        .get(version)
        .ok()
        .flatten()
        .map(|c| c.body)
        .unwrap_or(body);

    let parts: Vec<Part> = goal
        .parts
        .iter()
        .map(|p| Part {
            name: p.name.clone(),
            plan: json!({
                "text": p.text,
                "writable": p.writable,
                // Its OWN files — what it may write, as it stands — plus whatever
                // it is shown and may not write. Not the goal's top-level context:
                // for a decomposed goal that is usually empty, and a part handed
                // nothing writes blind.
                "context": p.writable.iter().chain(p.context.iter())
                    .filter_map(|f| std::fs::read_to_string(args.checkout.join(f))
                        .ok()
                        .map(|c| json!({ "path": f, "content": c })))
                    .collect::<Vec<_>>(),
                "previous": [],
                "checks": p.checks.iter().map(|c| json!({
                    "id": c.id, "required": c.required, "weight": c.weight, "command": c.command,
                })).collect::<Vec<_>>(),
                "base_commit": base_commit,
                "base_tree": tree,
                "max_attempts": args.attempts,
                "seed": 1,
            }),
        })
        .collect();
    println!(
        "parts: {}",
        parts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
    );

    let timeout = Duration::from_secs(args.timeout);
    let bounds =
        Bounds { branches: args.branches, max_rounds: args.rounds, max_tokens: 0, patience: 0 };
    let seed = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_secs();

    // The loop itself is library code, so the e2e that covers it drives THIS and
    // not a re-spelling of it. What is left here is what a binary is for: saying
    // what happened, and landing it.
    let run = compose::run_parts(
        &compose::Wiring {
            driver_url: &format!("http://127.0.0.1:{port}/run"),
            driver_host: "goalrun.acme.test",
            checks_url: &format!("http://127.0.0.1:{checks_port}/check"),
            registry: &registry,
            answerer: Some(&answerer),
        },
        &parts,
        &body,
        version,
        bounds,
        seed,
        timeout,
        base_commit,
        &json!(tree),
        &json!(composition_checks),
    );

    for line in &run.log {
        println!("  · {line}");
    }
    for p in &run.composition.parts {
        println!(
            "  {:<10} accepted={:<5} score={:<5} generations={} against contract v{}",
            p.part,
            p.accepted,
            p.best.as_ref().map(|b| b.score).unwrap_or(0),
            p.rounds.len(),
            p.built_against
        );
    }
    println!(
        "\nsearch: {:?}, {} tokens across {} part(s)",
        run.composition.stopped,
        run.composition.spent_tokens,
        run.composition.parts.len()
    );

    if !run.landable() {
        println!("\nNo PR opened:");
        for b in &run.blocked {
            println!("  · {b}");
        }
        // Which check, and what it said. "component never passed its gate" without
        // this is the least actionable sentence a run can end with — the ordinary
        // path has printed its closest failing checks since it existed.
        for p in &run.composition.parts {
            if let Some(best) = p.best.as_ref().filter(|_| !p.accepted) {
                if !best.failures.as_array().map(|a| a.is_empty()).unwrap_or(true) {
                    println!("\n  {} was still failing:", p.part);
                    for f in best.failures.as_array().unwrap() {
                        println!(
                            "    · {}: {}",
                            f["id"].as_str().unwrap_or("?"),
                            f["detail"].as_str().unwrap_or("").lines().take(6)
                                .collect::<Vec<_>>().join("\n      ")
                        );
                    }
                }
            }
        }
        return Ok(());
    }
    let report = run.report.as_ref().expect("landable means the gate ran");
    let changes = run.changes.clone().expect("landable means there is a tree");
    println!("  composition PASSED at score {}", report.score);

    if args.dry_run {
        println!("\n[dry run] the join passed; not opening a PR.");
        return Ok(());
    }

    // One pull request, carrying every part's work and the negotiation that got
    // them there — the part a reviewer most needs and could never reconstruct.
    let title = goal.title.clone().unwrap_or_else(|| {
        goal.text.lines().next().unwrap_or("a composed candidate").to_string()
    });
    let history = if run.log.is_empty() {
        "The parts needed nothing from each other.".to_string()
    } else {
        run.log.iter().map(|l| format!("- {l}")).collect::<Vec<_>>().join("\n")
    };
    let landing = json!({
        "base": args.base,
        "branch": format!("comp/goal-{}", SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_secs()),
        "title": title,
        "body": format!(
            "Automated candidate from a decomposed graph-engineering run.\n\n\
             {} part(s) built against contract v{}; the join passed at score {}.\n\n\
             ## Goal\n\n{}\n\n## How the interface got there\n\n{}\n",
            run.composition.parts.len(), run.composition.contract_version, report.score,
            goal.text, history
        ),
        "message": title,
    });

    // ONE candidate: the merged tree. The selector picks between branches, and a
    // composed run has already chosen — per part, and then joined.
    let joined = Entry {
        branch: "composition".into(),
        accepted: true,
        score: report.score,
        digest: String::new(),
        spent_tokens: run.composition.spent_tokens,
        attempts: run.composition.rounds_run as u64,
        files: changes,
        failures: json!([]),
        note: String::new(),
        elapsed_ms: 0,
        stopped: "accepted".into(),
    };
    println!("\nopening a pull request on {} …", args.repo);
    match land(
        &format!("http://127.0.0.1:{port}/land"),
        "goalland.acme.test",
        &[joined],
        landing,
        timeout,
    ) {
        Ok(v) if v["url"].is_string() => {
            println!("\n  PR opened: {}", v["url"].as_str().unwrap());
            println!("  branch: {}  commit: {}", v["branch"], v["commit"]);
        }
        Ok(v) => println!("\n  the forge answered but opened no PR: {v}"),
        Err(e) => println!("\n  landing failed: {e}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{component_scope, trim_members, GoalSpec};

    /// The goal spec for a decomposed run, as a person writes it.
    ///
    /// Three things are being asserted at once, and each is a decision: the parts
    /// carry their OWN checks (each half gates alone, against the contract), the
    /// top-level `[[check]]` list becomes the COMPOSITION gate (the checks that
    /// belong to the whole and can only run over the joined tree), and `contract`
    /// names a file in the checkout rather than being written inline — a person
    /// edits an interface in an editor that understands it.
    #[test]
    fn a_goal_can_be_two_parts_and_a_contract() {
        let spec: GoalSpec = toml::from_str(
            r#"
text = "Add a paged search box: a backend route and a frontend that renders it."
title = "Paged search across both halves"
contract = "CONTRACT.json"
writable = []

[[part]]
name = "backend"
text = "Serve GET /api/search over the corpus, exactly as CONTRACT.md describes."
writable = ["src/api.rs"]
[[part.check]]
id = "backend-serves-the-route"
command = ["grep", "-q", "/api/search", "src/api.rs"]

[[part]]
name = "frontend"
text = "Render the results with a pager, against the fixtures in .contract-mocks."
writable = ["ui/app.ts", "CONTRACT-REQUEST.md"]
[[part.check]]
id = "pager-renders"
command = ["grep", "-q", "pager", "ui/app.ts"]

[[check]]
id = "the-join"
command = ["grep", "-q", "total_pages", "ui/app.ts"]
"#,
        )
        .expect("a decomposed goal spec");

        assert_eq!(spec.contract.as_deref(), Some("CONTRACT.json"));
        let names: Vec<&str> = spec.parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["backend", "frontend"]);
        assert_eq!(spec.parts[0].checks.len(), 1, "each half gates alone");
        assert_eq!(spec.checks.len(), 1, "and the top-level checks judge the join");
        assert_eq!(spec.checks[0].id, "the-join");
        // `required` and `weight` default, as they do for an ordinary goal.
        assert!(spec.parts[1].checks[0].required);
        assert_eq!(spec.parts[1].checks[0].weight, 1);
        // The parts must not share a writable path — `compose::merge` refuses it,
        // and a spec that violates it has a decomposition bug, not a merge bug.
        assert!(
            spec.parts[0].writable.iter().all(|w| !spec.parts[1].writable.contains(w)),
            "parts must write disjoint paths"
        );
    }

    /// An ordinary goal is unchanged: no parts, no contract, and every existing
    /// spec in the repo still parses.
    #[test]
    fn a_goal_without_parts_is_the_path_it_always_was() {
        let spec: GoalSpec = toml::from_str(
            r#"
text = "make the answer 42"
writable = ["src/lib.rs"]
[[check]]
id = "tests"
command = ["cargo", "test"]
"#,
        )
        .expect("an ordinary goal spec");
        assert!(spec.parts.is_empty());
        assert!(spec.contract.is_none());
    }

    #[test]
    fn a_component_name_derives_its_build_scope() {
        let (base_paths, manifest, members) = component_scope("rot13");
        assert_eq!(base_paths, ["components/rot13/", "components/Cargo.toml"]);
        assert_eq!(manifest, "components/Cargo.toml");
        assert_eq!(members, ["rot13"], "the workspace is trimmed to just this crate");
    }

    #[test]
    fn trimming_keeps_the_rest_of_the_manifest() {
        let m = "[workspace]\nmembers = [\"a\", \"b\", \"c\"]\nresolver = \"2\"\n";
        let out = trim_members(m, &["b".to_string()]);
        assert!(out.contains("members = [\"b\"]"), "trimmed to the one member");
        assert!(out.contains("resolver = \"2\""), "the rest survives");
    }
}
