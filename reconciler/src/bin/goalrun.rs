//! `comp-goalrun` — take a goal to a pull request, for real.
//!
//! This is the binary behind `comp goal run`: the one command that turns a goal
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
//! the crate that owns the fleet, and `comp goal run` shells to it.
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
use comp_reconciler::generation::{search, land, Bounds, Entry};
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
    for (id, file) in [
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
    let mut allow: Vec<&str> =
        goal.checks.iter().filter_map(|c| c.command.first().map(String::as_str)).collect();
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
    let secrets = vec![
        format!("vault://acme/anthropic=@{}", args.anthropic_key.display()),
        format!("vault://acme/forge=@{}", args.github_token.display()),
    ];

    let specs = vec![driver_spec.to_str().unwrap().to_string(), forge_spec.to_str().unwrap().to_string()];
    let spec_refs: Vec<&str> = specs.iter().map(String::as_str).collect();
    let art = artifacts()?;

    println!("starting fleet …");
    let fleet = Fleet::start_with_secrets("goalrun", &spec_refs, &art, &secrets);
    let port = fleet.ingress_port;

    wait_serving(port, "goalrun.acme.test", Duration::from_secs(180))?;
    wait_serving(port, "goalland.acme.test", Duration::from_secs(180))?;

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
        println!("\nWhat smoke does NOT check (needs a real call, costs money):");
        println!("  · that the Anthropic key VALUE is accepted");
        println!("  · that the GitHub token VALUE can open a PR");
        println!("  · that `{}` actually runs under the gate", allow.join(" "));
        println!("\nRun for real by dropping --smoke.");
        return Ok(());
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

    let driver_url = format!("http://127.0.0.1:{port}/run");
    let timeout = Duration::from_secs(args.timeout);
    let bounds = Bounds { branches: args.branches, max_rounds: args.rounds, max_tokens: 0, patience: 0 };

    // A fresh seed each run so branches are not a replay of a previous run's.
    let seed = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_secs();
    let found = search(&driver_url, "goalrun.acme.test", &plan, bounds, seed, timeout);

    // Every attempt of every branch, so the run is legible even when it fails.
    let mut entries: Vec<Entry> = Vec::new();
    for (r, round) in found.rounds.iter().enumerate() {
        for e in &round.entries {
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
    use super::{component_scope, trim_members};

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
