//! Behind `holon goal run`: a goal to an opened PR. Keys arrive as file paths: argv is in `ps`.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use clap::Parser;
// `#[path]`: a bare `mod pool` would resolve to `src/bin/pool.rs`, a second binary target.
#[path = "goalrun/candidate.rs"]
mod candidate;
#[path = "goalrun/context.rs"]
mod context;
#[path = "goalrun/manifest.rs"]
mod manifest;
#[path = "goalrun/pool.rs"]
mod pool;
#[path = "goalrun/setup.rs"]
mod setup;

use candidate::{base_tree, gate_can_judge, head_commit, new_capabilities, smoke, write_candidate};
use context::{branch_context, first_line, join_failure_owners, part_context};
use manifest::{component_scope, trim_members};
use pool::reading_per_branch;
use setup::{host_bin, seed_capability_graph, set_fleet_timeouts, wait_serving};

use comp_fleet::fleet::Fleet;
use comp_goal::compose;
use comp_goal::contract::{Answerer, Registry};
use comp_goal::gate::Gate;
use comp_goal::generation as generation_mod;
use comp_goal::generation::{land, Bounds, Entry, Part};
use comp_goal::goalexit;
use comp_goal::memory::{self, run_id, Memory};
use comp_goal::trace::Trace;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "comp-goalrun", about = "Run a goal to a pull request, for real.")]
pub struct Args {
    /// The local checkout of the target repo; its tracked files are the base tree.
    #[arg(long)]
    checkout: PathBuf,
    /// `owner/name` of the repository the PR opens on.
    #[arg(long)]
    repo: String,
    /// The goal file, relative to the checkout.
    #[arg(long, default_value = ".comp/goal.toml")]
    goal: PathBuf,
    /// The branch to open the PR against.
    #[arg(long, default_value = "main")]
    base: String,
    /// A file holding the model API key, never argv; any content for a server that ignores auth.
    #[arg(long, alias = "anthropic-key")]
    llm_key: PathBuf,
    /// A file holding the GitHub token. Read here, never placed in argv.
    #[arg(long)]
    github_token: PathBuf,
    /// Opt-in SurrealDB for the knowledge pool, e.g. `http://127.0.0.1:8000` (ADR-0080).
    #[arg(long)]
    surreal_url: Option<String>,
    /// The database password, as a file path; absent means unauthenticated.
    #[arg(long)]
    surreal_password: Option<PathBuf>,
    /// Opt-in `capability-advisor` URL that vets pool hits; absent or unreachable trusts every hit.
    #[arg(long)]
    capability_advisor_url: Option<String>,
    /// Forget pool entries unread for this many days; 0 turns decay off.
    #[arg(long, default_value = "30")]
    forget_after_days: u32,
    /// Seconds any single gate check may take; a timeout reads as a candidate failure, so raise it
    /// for gates that build and call a model.
    #[arg(long, env = "CHECK_TIMEOUT", default_value = "120")]
    check_timeout: u64,
    /// Use an already-running `comp-checks` instead of starting one; its authority joins the gate's
    /// egress allow-list (ADR-0008).
    #[arg(long)]
    checks_url: Option<String>,
    /// A file holding the bearer token for `--checks-url`; required when that is not loopback.
    #[arg(long)]
    checks_token_file: Option<PathBuf>,
    /// Skip the search when a past passing run is this similar (cosine); high on purpose.
    #[arg(long, default_value = "0.9")]
    skip_above: f64,
    /// The writer's token budget per attempt; a thinking model spends it before answering.
    #[arg(long, default_value = "32000")]
    max_tokens: u32,
    /// The model answering a part's request at a generation boundary; cheap by default (ADR-0086).
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
    /// Base URL of the model API; empty uses the provider's default. A private address also needs
    /// `COMP_FLEET_ALLOW_PRIVATE_EGRESS=1`.
    #[arg(long, alias = "anthropic-base-url", default_value = "")]
    llm_base_url: String,
    /// Which provider answers the writer: `anthropic` (`/v1/messages`, the Claude shim) or `openai`
    /// (`/v1/chat/completions`: vLLM, llama.cpp, Ollama, mlx).
    #[arg(long, value_parser = ["anthropic", "openai"], default_value = "anthropic")]
    provider: String,
    /// Per-branch HTTP timeout in seconds; it must hold `attempts` sequential model calls.
    /// Running out reads as `error sending request`, not as a timeout.
    #[arg(long, default_value_t = 900)]
    timeout: u64,
    /// Open the PR at the end; off is a dry run.
    #[arg(long, default_value_t = false)]
    dry_run: bool,
    /// Bring the fleet up, confirm both apps serve, and exit before any model call.
    #[arg(long, default_value_t = false)]
    smoke: bool,
}

#[derive(Deserialize)]
pub struct GoalSpec {
    text: String,
    writable: Vec<String>,
    #[serde(default)]
    title: Option<String>,
    /// Path prefixes of the base tree; empty is the whole repo (NATS caps a message near 1 MB).
    #[serde(default)]
    base_paths: Vec<String>,
    /// Trimmed to `keep_members` so one crate of a shared workspace builds standalone.
    #[serde(default)]
    workspace_manifest: Option<String>,
    #[serde(default)]
    keep_members: Vec<String>,
    /// Derives `base_paths`, `workspace_manifest` and `keep_members` from `components/<name>/`;
    /// explicit fields win.
    #[serde(default)]
    component: Option<String>,
    /// Shown to a branch but not writable (only `writable` grants writes): its held-out tests.
    #[serde(default)]
    context: Vec<String>,
    #[serde(rename = "check")]
    checks: Vec<CheckSpec>,
    /// The interface both parts build against; required for a decomposed goal (ADR-0086).
    #[serde(default)]
    contract: Option<String>,
    /// Empty means an ordinary goal; with parts, the top-level checks are the composition gate.
    #[serde(default, rename = "part")]
    parts: Vec<PartSpec>,
}

#[derive(Deserialize)]
struct PartSpec {
    name: String,
    text: String,
    /// Disjoint from every other part's, or the merge refuses it.
    writable: Vec<String>,
    /// Shown but not writable; without its tests a part writes blind.
    #[serde(default)]
    context: Vec<String>,
    /// This half's own gate, run against the contract alone so neither part waits for the other.
    #[serde(rename = "check")]
    checks: Vec<CheckSpec>,
}

#[derive(Deserialize)]
struct CheckSpec {
    id: String,
    /// Supposed to be green on the untouched base: a regression test, an invariant already true.
    #[serde(default)]
    may_pass_base: bool,
    #[serde(default = "yes")]
    required: bool,
    #[serde(default = "one")]
    weight: u32,
    command: Vec<String>,
    /// Check ids that must pass first, so a non-compiling candidate fails once (ADR-0088).
    #[serde(default)]
    needs: Vec<String>,
}
fn yes() -> bool {
    true
}
fn one() -> u32 {
    1
}

/// Returns the exit code rather than calling `exit`, which skips destructors and leaked the fleet.
fn run() -> Result<i32> {
    let args = Args::parse();

    let goal_path = args.checkout.join(&args.goal);
    let mut goal: GoalSpec = toml::from_str(
        &std::fs::read_to_string(&goal_path)
            .with_context(|| format!("reading {}", goal_path.display()))?,
    )
    .with_context(|| format!("parsing {}", goal_path.display()))?;
    if goal.checks.is_empty() {
        bail!("the goal has no checks — an empty gate accepts everything");
    }
    // An explicitly set field wins over the derived scope.
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

    let mut tree = base_tree(&args.checkout, &goal.base_paths)?;
    if let (Some(manifest), false) = (&goal.workspace_manifest, goal.keep_members.is_empty()) {
        for e in tree.iter_mut() {
            if e["path"] == serde_json::json!(manifest) {
                let trimmed =
                    trim_members(e["content"].as_str().unwrap_or_default(), &goal.keep_members);
                e["content"] = serde_json::json!(trimmed);
            }
        }
    }
    let base_commit = head_commit(&args.checkout)?;
    let context = branch_context(&args.checkout, &goal.writable, &goal.context);

    let checks: Vec<Value> = goal
        .checks
        .iter()
        .map(|c| json!({ "id": c.id, "required": c.required, "weight": c.weight, "command": c.command, "needs": c.needs }))
        .collect();

    // Every check's first word, the parts' too: the top-level list is only the composition gate.
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
    // `allow` only reaches a runner this process starts; a remote one has its own `--allow`.
    match &args.checks_url {
        None => println!("gate allows: {allow:?}"),
        Some(u) => println!(
            "gate needs: {allow:?} — and this run does not set that: {u} was started by \
             somebody else, with its own --allow"
        ),
    }
    // Warned, not corrected: a silently raised timeout is its own surprise. 150s is the Claude CLI
    // shim's slow tail.
    let needed = args.attempts as u64 * 150 + 30;
    if args.timeout < needed {
        println!(
            "WARNING: --timeout {}s cannot hold {} sequential model calls (~{}s needed).\n                      A branch that runs out dies with `error sending request` and NO attempts.\n                      Raise --timeout or lower --attempts.",
            args.timeout, args.attempts, needed
        );
    }
    let hb = host_bin();
    println!(
        "gate host:   {} ({})",
        hb.display(),
        if hb.exists() { "found" } else { "MISSING — every check that runs the app will fail" }
    );

    let check_env = setup::warm_caches(&goal, &args);

    let gate = Gate::open(
        args.checks_url.as_deref(),
        args.checks_token_file.as_deref(),
        args.check_timeout,
        &allow,
        &check_env,
    )?;

    set_fleet_timeouts(&args);
    let setup::Deployment { specs, secrets, artifacts: art } =
        setup::render_specs(&args, &goal, &gate)?;
    let spec_refs: Vec<&str> = specs.iter().map(String::as_str).collect();

    println!("starting fleet …");
    let fleet = Fleet::start_with_secrets("goalrun", &spec_refs, &art, &secrets);
    let port = fleet.ingress_port;

    wait_serving(port, "goalrun.acme.test", Duration::from_secs(180))?;
    wait_serving(port, "goalland.acme.test", Duration::from_secs(180))?;

    // An app that will not serve is reported and ignored: memory must never stop a run.
    let memory = match &args.surreal_url {
        None => None,
        Some(url) => match wait_serving(port, "goalmemory.acme.test", Duration::from_secs(180)) {
            Ok(()) => {
                println!("knowledge pool serving, backed by {url}");
                let pw = args
                    .surreal_password
                    .as_ref()
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .map(|s| s.trim().to_string());
                seed_capability_graph(url, pw.as_deref());
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

    // Seeded so plans name the commit, not 500 KB of tree. Before the critic, which may fail.
    if let Err(e) = compose::seed_base(
        &gate.url(),
        gate.token().as_deref(),
        &base_commit,
        &json!(tree),
        Duration::from_secs(args.timeout),
    ) {
        bail!(
            "could not give the gate runner the base tree: {e}\n\n\
             Nothing was spent. Every branch would have failed identically, because \
             the plan carries the commit and the runner is what holds the bytes."
        );
    }
    println!(
        "gate: base {} seeded ({} files)",
        &base_commit[..base_commit.len().min(8)],
        tree.len()
    );

    // Criticise the gate before the money.
    if !gate_can_judge(&goal, &checks, &gate, &base_commit, &tree, args.timeout) {
        // 4, not 0: a gate that could not judge spent nothing. Distinct from EXHAUSTED (3).
        return Ok(goalexit::GATE_REFUSED);
    }

    if args.smoke {
        return smoke(&args, &goal, port, &context, &checks, &base_commit, &allow)
            .map(|()| goalexit::SUCCESS);
    }

    if !pool::worth_running(memory.as_ref(), &goal, args.skip_above) {
        return Ok(goalexit::SUCCESS);
    }

    // Before the decomposed dispatch, which once returned before any trace existed (ADR-0092).
    let seed = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_secs();
    let run = seed.to_string();
    // By value: the trace talks to the database directly.
    let surreal_password = args
        .surreal_password
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string());
    let trace = args.surreal_url.as_deref().map(|url| {
        // Same database as the graph component, so runs and lessons join (ADR-0091).
        Trace::new(url, "goalmemory", surreal_password.as_deref())
    });
    if let Some(t) = &trace {
        t.run_started(
            &run,
            &goal.text,
            &args.goal.display().to_string(),
            seed,
            &base_commit,
            args.branches.into(),
        );
    }

    // Above the decomposed dispatch: a part is where reuse is likeliest (ADR-0094).
    let (reuse, capability_confirmed) = pool::search_the_pool(
        &goal.text,
        &run,
        trace.as_ref(),
        args.capability_advisor_url.as_deref(),
    );

    // Decomposed: parts compose rather than compete, judged as one merged tree (ADR-0086).
    if !goal.parts.is_empty() {
        return decomposed(
            &args,
            &goal,
            port,
            &gate,
            memory.clone(),
            &context,
            &tree,
            &base_commit,
            &checks,
            seed,
            trace.as_ref(),
            &reuse,
            capability_confirmed.as_ref(),
        );
    }

    println!("fleet serving; running the search …\n");

    let plan = json!({
        "text": goal.text,
        "writable": goal.writable,
        "context": context,
        "previous": [],
        "checks": checks,
        "base_commit": base_commit,
        // Not the tree: the runner is seeded and keyed by commit, saving 500 KB per branch.
        "base_tree": [],
        "max_attempts": args.attempts,
        "seed": 1,
    });

    let (strategies, read_by_branch) = reading_per_branch(&args, &goal, memory.as_ref());

    let driver_url = format!("http://127.0.0.1:{port}/run");
    let timeout = Duration::from_secs(args.timeout);
    let bounds =
        Bounds { branches: args.branches, max_rounds: args.rounds, max_tokens: 0, patience: 0 };

    let mut plan = plan;
    if let Some(entry) = pool::pool_context(&reuse, capability_confirmed.as_ref()) {
        if let Some(ctx) = plan["context"].as_array_mut() {
            ctx.push(entry);
        }
    }

    let found = generation_mod::search_with(
        &driver_url,
        "goalrun.acme.test",
        &plan,
        &strategies,
        bounds,
        seed,
        timeout,
    );

    let mut entries: Vec<Entry> = Vec::new();
    let mut recorded = 0usize;
    // `search` picks the winning entry, but only this walk knows its run id.
    let mut winner: Option<(String, u64)> = None;
    for (r, round) in found.rounds.iter().enumerate() {
        for e in &round.entries {
            // One verdict per branch; the landing path re-reports the winner with its artifact.
            let attempt = run_id(seed, r, &e.branch);
            if e.accepted && winner.as_ref().is_none_or(|(_, best)| e.score > *best) {
                winner = Some((attempt.clone(), e.score));
            }
            if let Some(t) = &trace {
                // Recorded together because this walk runs after the search.
                t.branch_spawned(&run, &attempt, &e.branch, r);
                t.gate_verdict(&run, &attempt, e.score, e.accepted, &e.failures);
                t.attempt_finished(
                    &run,
                    &attempt,
                    if e.accepted { "passed" } else { "failed" },
                    e.score,
                    &e.files,
                    e.spent_tokens,
                    e.elapsed_ms,
                    e.attempts,
                );
            }
            if let Some(m) = &memory {
                match m.evaluated(&goal.text, &attempt, e.score, e.accepted, "") {
                    Ok(()) => recorded += 1,
                    Err(err) => println!("  (verdict for {attempt} not recorded: {err})"),
                }
                // No model in the path, so a lesson cannot be a hallucination (ADR-0081).
                if !e.accepted {
                    if let Some(text) = memory::failure_text(&e.failures, e.score) {
                        match m.observe_failure(&goal.text, &e.branch, &attempt, &text) {
                            Ok(h) => println!("  {} wrote a lesson: {h}", e.branch),
                            Err(err) => println!("  (lesson from {run} not recorded: {err})"),
                        }
                    }
                }
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

    // A branch that never ran or produced nothing died before the model; the reason is only in the
    // host and ingress logs, which the fleet's tempdir drops on exit.
    if entries.iter().any(|e| !e.note.is_empty() || (e.spent_tokens == 0 && !e.accepted)) {
        let tail = |s: &str, n: usize| {
            let lines: Vec<&str> = s.lines().collect();
            lines[lines.len().saturating_sub(n)..].join("\n")
        };
        eprintln!("\n===== host n1 (last 60 lines) =====\n{}", tail(&fleet.node_log("n1"), 60));
        eprintln!("\n===== ingress (last 25 lines) =====\n{}", tail(&fleet.ingress_log(""), 25));
    }

    pool::promote_and_sweep(
        memory.as_ref(),
        &args,
        &goal,
        port,
        found.best.as_ref(),
        &winner.as_ref().map(|(r, _)| r.clone()).unwrap_or_default(),
    );
    // From the winner's paths: only a component that passed a gate is a capability (ADR-0089).
    if let (Some(t), Some(best)) = (&trace, found.best.as_ref()) {
        if best.accepted {
            for (name, path) in new_capabilities(&best.files) {
                t.capability_added(&run, &name, &path);
            }
        }
    }

    if !found.accepted() {
        let best = found.best.as_ref().map(|e| e.score).unwrap_or(0);
        println!("\nNothing passed the gate (best score {best}). No PR opened.");
        if let Some(b) = &found.best {
            println!("closest failing checks: {}", b.failures);
        }
        // `exhausted`, not `failed`: every branch ran and none passed.
        if let Some(t) = &trace {
            t.run_resolved(&run, "exhausted", None, "");
            if let Some(why) = t.report() {
                println!("trace: {why}");
            }
        }
        // 3: not 0 (goald read that as awaiting-human), not 1 (a broken run).
        return Ok(goalexit::EXHAUSTED);
    }

    if args.dry_run {
        let best = found.best.as_ref().unwrap();
        println!("\n[dry run] a candidate passed (score {}); not opening a PR.", best.score);
        // Written out, to a directory: a dry run keeps its winner without mutating the checkout.
        match write_candidate(&args.checkout, &best.files) {
            Ok(dir) => {
                println!("  the winning files are in {}", dir.display());
                println!("  apply:   rsync -a {}/ .", dir.display());
                println!("  inspect: diff -ru . {} | head -n 100", dir.display());
            }
            Err(e) => println!("  WARNING: the winner could not be written out ({e}) — it is lost"),
        }
        if let Some(t) = &trace {
            t.run_resolved(&run, "dry-run", winner.as_ref().map(|(w, _)| w.as_str()), "");
            if let Some(why) = t.report() {
                println!("trace: {why}");
            }
        }
        return Ok(goalexit::SUCCESS);
    }

    // A unique branch name per run: a PR cannot reuse one.
    let title = goal
        .title
        .clone()
        .unwrap_or_else(|| goal.text.lines().next().unwrap_or("a candidate").to_string());
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
    let landed = match land(&select_url, "goalland.acme.test", &entries, landing, timeout) {
        Ok(v) if v["url"].is_string() => {
            let url = v["url"].as_str().unwrap();
            println!("\n  PR opened: {url}");
            println!("  branch: {}  commit: {}", v["branch"], v["commit"]);
            // Re-reported now that the run is addressable; idempotent per (goal, run).
            if let (Some(m), Some(w)) = (&memory, &winner) {
                if let Err(e) = m.evaluated(&goal.text, &w.0, w.1, true, url) {
                    println!("  (the pull request was not recorded against the goal: {e})");
                }
            }
            if let Some(t) = &trace {
                t.run_resolved(&run, "merged", winner.as_ref().map(|(w, _)| w.as_str()), url);
            }
            goalexit::SUCCESS
        }
        Ok(v) => {
            println!("\n  the forge answered but opened no PR: {v}");
            // Failed, not exhausted: conflating them would hide the forge as the cause.
            if let Some(t) = &trace {
                t.run_resolved(&run, "failed", winner.as_ref().map(|(w, _)| w.as_str()), "");
            }
            goalexit::LANDING_FAILED
        }
        Err(e) => {
            println!("\n  landing failed: {e}");
            if let Some(t) = &trace {
                t.run_resolved(&run, "failed", winner.as_ref().map(|(w, _)| w.as_str()), "");
            }
            goalexit::LANDING_FAILED
        }
    };
    // One line at the end: per-write reports would drown the output when the database is down.
    if let Some(why) = trace.as_ref().and_then(|t| t.report()) {
        println!("trace: {why}");
    }
    Ok(landed)
}

/// Exits only after `run()` dropped its fleet and children; `exit` skips destructors.
fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("Error: {e:?}");
            std::process::exit(1);
        }
    }
}

/// One goal, K parts, one PR; nothing lands until every part is green.
#[allow(clippy::too_many_arguments)]
fn decomposed(
    args: &Args,
    goal: &GoalSpec,
    port: u16,
    gate: &Gate,
    memory_for_parts: Option<Memory>,
    // Unused on purpose: a part sees its own files plus its `context`, never the goal's list.
    _context: &[Value],
    tree: &[Value],
    base_commit: &str,
    composition_checks: &[Value],
    seed: u64,
    trace: Option<&Trace>,
    // Searched by the caller so a run asks the catalogue once (ADR-0094).
    reuse: &[comp_goal::capsearch::Capability],
    // `None` means every hit is trusted unfiltered.
    capability_confirmed: Option<&std::collections::BTreeSet<String>>,
) -> Result<i32> {
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

    let contract_path = goal.contract.clone().unwrap_or_default();
    let body = std::fs::read_to_string(args.checkout.join(&contract_path))
        .with_context(|| format!("reading the contract at {contract_path}"))?;
    // A repeat run (smoke then real) finds its own contract stored; refuse only when it differs.
    let version = match registry.publish(&body) {
        Ok(v) => {
            println!("contract v{v} published from {contract_path}");
            v
        }
        Err(e) if e.contains("already published") => {
            let current = registry
                .current()
                .map_err(|e| anyhow::anyhow!("a contract is registered but unreadable: {e}"))?;
            if current.body.trim() != body.trim() {
                bail!(
                    "the registry holds a contract v{} that is not what {contract_path} says.\n\n\
                     If an earlier run amended it, that is the amendment and the file is stale — \
                     amendments are made through ask/answer, not by editing the file. If this is \
                     a different goal, it wants a database of its own.\n\n\
                     registered: {}\n\n\
                     the file:   {}",
                    current.number,
                    current
                        .body
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .take(90)
                        .collect::<String>(),
                    body.lines().next().unwrap_or_default().chars().take(90).collect::<String>(),
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
    // Canonical now, which after an earlier run's negotiation may be later than v1.
    let body = registry.get(version).ok().flatten().map(|c| c.body).unwrap_or(body);

    // Every part's: a part that reimplements `auth-guard` fails ADR-0089's gate anyway.
    let pool_entry = pool::pool_context(reuse, capability_confirmed);

    let parts: Vec<Part> = goal
        .parts
        .iter()
        .map(|p| {
            let context =
                part_context(&args.checkout, &p.writable, &p.context, pool_entry.clone());
            Part {
            name: p.name.clone(),
            plan: json!({
                "text": p.text,
                "writable": p.writable,
                // Its own files plus read-only context; the goal's top-level list is usually empty.
                "context": context,
                "previous": [],
                "checks": p.checks.iter().map(|c| json!({
                    "id": c.id, "required": c.required, "weight": c.weight, "command": c.command,
                    "needs": c.needs,
                })).collect::<Vec<_>>(),
                "base_commit": base_commit,
                // Empty: `main` seeded the runner once; the tree would cross the lattice K times.
                "base_tree": [],
                "max_attempts": args.attempts,
                "seed": 1,
            }),
            }
        })
        .collect();
    println!("parts: {}", parts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", "));

    // Before the search, so the pool also sees bad decompositions; never fatal (ADR-0084).
    if let Some(m) = memory_for_parts.as_ref() {
        let mut written = 0usize;
        for (i, p) in goal.parts.iter().enumerate() {
            match m.decomposed_into(&goal.text, &p.text, i as u32, &p.name) {
                Ok(()) => written += 1,
                Err(e) => println!("knowledge: part `{}` not linked to its goal ({e})", p.name),
            }
        }
        if written > 0 {
            println!(
                "knowledge: {written}/{} part(s) linked to this goal — a later run can ask \
                 what it broke into",
                goal.parts.len()
            );
        }
    }

    let timeout = Duration::from_secs(args.timeout);
    let bounds =
        Bounds { branches: args.branches, max_rounds: args.rounds, max_tokens: 0, patience: 0 };
    let run_key = seed.to_string();
    // Every return goes through this, so no run row is left started and never resolved.
    let resolve = |outcome: &str, url: &str| {
        if let Some(t) = trace {
            // "composition": the join passed; naming one part's branch would credit half the work.
            let winner = (outcome == "merged").then_some("composition");
            t.run_resolved(&run_key, outcome, winner, url);
            if let Some(why) = t.report() {
                println!("\ntrace: {why}");
            }
        }
    };

    // Library code, so the e2e drives this and not a re-spelling of it.
    let run = compose::run_parts(
        &compose::Wiring {
            driver_url: &format!("http://127.0.0.1:{port}/run"),
            driver_host: "goalrun.acme.test",
            checks_url: &gate.url(),
            checks_token: gate.token().as_deref(),
            registry: &registry,
            answerer: Some(&answerer),
            memory: memory_for_parts.as_ref(),
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

    // Before anything can fail. The part is in the attempt id: both parts run `branch-0`.
    if let Some(t) = trace {
        for p in &run.composition.parts {
            for (r, round) in p.rounds.iter().enumerate() {
                for e in &round.entries {
                    let attempt = run_id(seed, r, &format!("{}/{}", p.part, e.branch));
                    t.branch_spawned(&run_key, &attempt, &e.branch, r);
                    t.gate_verdict(&run_key, &attempt, e.score, e.accepted, &e.failures);
                    // `errored`: nothing produced; `failed` is for candidates the gate judged.
                    let outcome = if e.accepted {
                        "passed"
                    } else if e.files.as_array().map(|a| a.is_empty()).unwrap_or(true) {
                        "errored"
                    } else {
                        "failed"
                    };
                    t.attempt_finished(
                        &run_key,
                        &attempt,
                        outcome,
                        e.score,
                        &e.files,
                        e.spent_tokens,
                        e.elapsed_ms,
                        e.attempts,
                    );
                }
            }
        }
    }

    for line in &run.log {
        println!("  · {line}");
    }
    for p in &run.composition.parts {
        println!(
            "  {:<16} accepted={:<5} score={:<5} generations={} against contract v{}",
            p.part,
            p.accepted,
            p.best.as_ref().map(|b| b.score).unwrap_or(0),
            p.rounds.len(),
            p.built_against
        );
        // The note is the only place a transport failure, refused plan or dead provider says so.
        for (r, round) in p.rounds.iter().enumerate() {
            for e in round.entries.iter().filter(|e| !e.note.is_empty()) {
                println!("      gen {r} {}: {}", e.branch, e.note);
            }
        }
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
        for p in &run.composition.parts {
            if let Some(best) = p.best.as_ref().filter(|_| !p.accepted) {
                if !best.failures.as_array().map(|a| a.is_empty()).unwrap_or(true) {
                    println!("\n  {} was still failing:", p.part);
                    for f in best.failures.as_array().unwrap() {
                        println!("    · {}: {}", f["id"].as_str().unwrap_or("?"), {
                            // The last lines: a failing command says what went wrong at the end.
                            let d = f["detail"].as_str().unwrap_or("");
                            let lines: Vec<&str> =
                                d.lines().filter(|l| !l.trim().is_empty()).collect();
                            lines[lines.len().saturating_sub(6)..].join("\n      ")
                        });
                    }
                }
            }
        }
        // A join failure: each half passed and the whole did not, so no part's gate explains it.
        if run.report.is_some() || run.changes.is_some() {
            for b in &run.blocked {
                let owners = join_failure_owners(b, &goal.parts);
                if owners.is_empty() {
                    println!("  · the JOIN owns this, not a part: {}", first_line(b));
                } else {
                    println!("  · owned by {}: {}", owners.join(" and "), first_line(b));
                }
            }
        }

        // Written out: a join failure otherwise discards every part's accepted code.
        if let Some(changes) = &run.changes {
            match write_candidate(&args.checkout, changes) {
                Ok(dir) => println!(
                    "\n  the merged tree is in {} (it did not pass the join)",
                    dir.display()
                ),
                Err(e) => println!("\n  WARNING: the merged tree could not be written out ({e})"),
            }
        }

        resolve("exhausted", "");
        return Ok(goalexit::EXHAUSTED);
    }
    let report = run.report.as_ref().expect("landable means the gate ran");
    let changes = run.changes.clone().expect("landable means there is a tree");
    println!("  composition PASSED at score {}", report.score);

    // Promotion is earned by the verdict, not the forge, so before the PR.
    pool::promote_parts(memory_for_parts.as_ref(), goal, port, &run.composition.parts);
    // From the merged tree: the join is what passed (ADR-0089).
    if let Some(t) = trace {
        for (name, path) in new_capabilities(&changes) {
            t.capability_added(&run_key, &name, &path);
        }
    }

    if args.dry_run {
        println!("\n[dry run] the join passed; not opening a PR.");
        match write_candidate(&args.checkout, &changes) {
            Ok(dir) => {
                println!("  the joined tree is in {}", dir.display());
                println!("  apply: rsync -a {}/ .", dir.display());
            }
            Err(e) => {
                println!("  WARNING: the joined tree could not be written out ({e}) — it is lost")
            }
        }
        resolve("dry-run", "");
        return Ok(goalexit::SUCCESS);
    }

    let title = goal
        .title
        .clone()
        .unwrap_or_else(|| goal.text.lines().next().unwrap_or("a composed candidate").to_string());
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

    // One candidate, the merged tree: a composed run has already chosen.
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
            let url = v["url"].as_str().unwrap();
            println!("\n  PR opened: {url}");
            println!("  branch: {}  commit: {}", v["branch"], v["commit"]);
            resolve("merged", url);
            Ok(goalexit::SUCCESS)
        }
        Ok(v) => {
            println!("\n  the forge answered but opened no PR: {v}");
            resolve("failed", "");
            Ok(goalexit::LANDING_FAILED)
        }
        Err(e) => {
            println!("\n  landing failed: {e}");
            resolve("failed", "");
            Ok(goalexit::LANDING_FAILED)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::GoalSpec;
    use comp_goal::gate::egress_authority;

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
        assert!(spec.parts[1].checks[0].required);
        assert_eq!(spec.parts[1].checks[0].weight, 1);
        assert!(
            spec.parts[0].writable.iter().all(|w| !spec.parts[1].writable.contains(w)),
            "parts must write disjoint paths"
        );
    }

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

    /// A wrong derivation fails at the first call with an egress error naming a host nobody typed.
    #[test]
    fn the_egress_authority_comes_from_the_base_url() {
        assert_eq!(egress_authority("https://api.anthropic.com"), "api.anthropic.com");
        // The port is kept: a bare `127.0.0.1` would allow every loopback port.
        assert_eq!(egress_authority("http://127.0.0.1:8787"), "127.0.0.1:8787");
        assert_eq!(egress_authority("http://localhost:8787/v1"), "localhost:8787");
        assert_eq!(egress_authority("https://proxy.internal/anthropic"), "proxy.internal");
        assert_eq!(egress_authority("http://[::1]:8787"), "[::1]:8787");
    }

    /// Both halves spawn `branch-0` in round 0; the second must not overwrite the first's row.
    #[test]
    fn two_parts_do_not_share_one_attempt_id() {
        use comp_goal::memory::run_id;
        assert_ne!(
            run_id(7, 0, "access-and-search/branch-0"),
            run_id(7, 0, "reports/branch-0"),
            "the part name must be part of the key"
        );
        assert_ne!(run_id(7, 0, "reports/branch-0"), run_id(7, 1, "reports/branch-0"));
        assert_ne!(run_id(7, 0, "reports/branch-0"), run_id(7, 0, "reports/branch-1"));
    }
}
