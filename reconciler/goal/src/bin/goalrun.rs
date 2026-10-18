//! `comp-goalrun` — take a goal to a pull request, for real.
//!
//! Binary behind `holon goal run`: assembles the separately tested pieces (search loop, model provider,
//! checks gate, forge) onto one fleet and drives them from a goal to an opened PR.
//!
//! `comp` is a thin HTTP client; a real run needs the whole substrate (NATS, hosts, the gate's native
//! runner), which `fleet::Fleet` stands up, so orchestration lives in the crate that owns the fleet.
//!
//! ## Secrets never touch argv
//!
//! The Anthropic key and GitHub token arrive as FILE PATHS and reach the vault as `vault://…=@path`;
//! a key on a command line is in every `ps` and shell history.
//!
//! ## What is real and what is not
//!
//! All of it is real except picking the goal off a queue: a person still runs the command, deliberately,
//! until the interruption rate is understood (ADR-0082).

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use clap::Parser;
// `#[path]`, because this file IS the crate root for the binary: a bare `mod pool`
// resolves to `src/bin/pool.rs`, which cargo would then discover as a SECOND
// binary target. The submodule lives in a directory named after its only caller.
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
    /// The local checkout of the target repo. Its tracked files are the base tree
    /// the candidates are judged against, and `.comp/goal.toml` is the goal.
    #[arg(long)]
    checkout: PathBuf,
    /// `owner/name` of the repository the PR opens on.
    #[arg(long)]
    repo: String,
    /// The goal file, relative to the checkout. Defaults to `.comp/goal.toml`.
    ///
    /// A queue holds MANY goals against one repository, and each is its own file
    /// in git. Without this, driving a queue means copying the next goal over
    /// `.comp/goal.toml` before every run — a mutation of the checkout that races
    /// the moment two runs overlap, and one that leaves the working tree dirty in
    /// a way the base tree then ships.
    #[arg(long, default_value = ".comp/goal.toml")]
    goal: PathBuf,
    /// The branch to open the PR against.
    #[arg(long, default_value = "main")]
    base: String,
    /// A file holding the model API key. Read here, never placed in argv.
    ///
    /// A local server that ignores auth still needs the file to exist; give it
    /// anything. `openai-provider` sends no header when the value is empty.
    #[arg(long, alias = "anthropic-key")]
    llm_key: PathBuf,
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
    /// A `capability-advisor` instance (see `components/capability-advisor`),
    /// e.g. `http://127.0.0.1:8300`.
    ///
    /// OPT-IN, and absent by default — same shape as `--surreal-url`. Given
    /// one, `search_the_pool`'s lexical hits each get one Jev question before
    /// they reach `POOL.md`; absent, or unreachable, every hit is trusted
    /// exactly as it is today. Never blocks a run either way.
    #[arg(long)]
    capability_advisor_url: Option<String>,
    /// Forget entries nothing has read in this many days. 0 turns decay off.
    ///
    /// Swept by the run that uses the pool, because a decay nothing drives is the
    /// gap ADR-0081 caught elsewhere and naming it does not close it.
    #[arg(long, default_value = "30")]
    forget_after_days: u32,
    /// Seconds any single gate check may take before it is killed.
    ///
    /// 120 is right for a check that runs a test suite against a warm target
    /// directory. It is NOT right for one that builds a composition out of nine
    /// crates and then makes a real model call — and a check killed on time is
    /// reported to the branch as a failure it did not cause, which poisons the
    /// feedback the next attempt reads. Raise it for goals whose gates do real work;
    /// `CHECK_TIMEOUT` in the environment sets it for a whole run.
    #[arg(long, env = "CHECK_TIMEOUT", default_value = "120")]
    check_timeout: u64,
    /// Use a `comp-checks` that is ALREADY RUNNING, instead of starting one here.
    ///
    /// This is what makes the gate a second machine's job. `comp-checks`
    /// materialises the candidate tree from the request, so the box on the other
    /// end needs no checkout of the project being gated and no toolchain beyond
    /// what the checks themselves name — which is the shape the runner was
    /// written for and, until this flag, the shape nothing could ask for.
    ///
    ///   --checks-url http://malna:8099/check --checks-token-file ~/.comp-secrets/checks
    ///
    /// The URL's authority also goes into the gate component's egress allow-list,
    /// because a component may only dial what the manifest names (ADR-0008).
    #[arg(long)]
    checks_url: Option<String>,
    /// A FILE holding the bearer token for `--checks-url`. Never a value.
    ///
    /// Required with a `--checks-url` that is not loopback, for the reason
    /// `comp-checks` refuses to listen off the loopback without one: `--allow`
    /// bounds the command, not the tree it runs over.
    ///
    /// Ignored when the runner is started here — that one gets a freshly minted
    /// token nobody has to manage.
    #[arg(long)]
    checks_token_file: Option<PathBuf>,
    /// Skip the whole search when a past passing run of a goal this similar is on
    /// record. Cosine; 0.9 is alpha-swarm2's and is high on purpose — redoing work
    /// costs money, skipping work that was never done is a wrong answer.
    #[arg(long, default_value = "0.9")]
    skip_above: f64,
    /// The writer's token budget per attempt.
    ///
    /// Not 4096: a THINKING model spends part of this before it writes anything,
    /// and on a real task it can spend all of it — measured on claude-sonnet-5,
    /// which returned `["thinking"]` and `stop_reason: max_tokens` at 4096, a
    /// complete file at 16000 on a small prompt, and STILL exhausted 16000 on a
    /// real one — a third of a clinic run's branches died there. A budget that is
    /// fine for one model is a silent wall for another, and thinking is bought
    /// out of the same purse as the answer.
    #[arg(long, default_value = "32000")]
    max_tokens: u32,
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
    /// Where `openai-provider` sends `/v1/chat/completions`.
    ///
    /// Anything that speaks the OpenAI JSON contract: the real API, vLLM, Together,
    /// Groq, llama.cpp, or a self-hosted mlx server on the next desk.
    ///
    ///   holon goal run --llm-base-url http://csatapaci:8080/v1 ...
    ///
    /// This used to name an Anthropic endpoint, and reaching a local OpenAI server
    /// meant a translating shim in front of it. `openai-provider` already
    /// implements the same `llm:inference` WIT contract, so the shim was a process
    /// and a timeout in the path for no capability the graph did not have.
    ///
    /// A private address additionally needs `COMP_FLEET_ALLOW_PRIVATE_EGRESS=1`
    /// — the fleet blocks egress to private ranges by default, and a base URL is
    /// exactly the knob an injected prompt would reach for.
    ///
    /// Defaults per provider: Anthropic's API for `anthropic`, OpenAI's for
    /// `openai`. Left empty, the provider's own default applies.
    #[arg(long, alias = "anthropic-base-url", default_value = "")]
    llm_base_url: String,
    /// Which provider component answers the writer.
    ///
    /// Both implement `llm:inference/inference`, so this picks a wasm artifact and
    /// a config key prefix and nothing else changes. It is a REAL choice rather
    /// than a swap because the two reach different servers: `anthropic` speaks
    /// `/v1/messages`, which is what `tools/claude-shim.mjs` serves to run the
    /// loop on a Claude Code subscription, and `openai` speaks
    /// `/v1/chat/completions`, which is what vLLM, llama.cpp, Ollama and a local
    /// mlx server serve directly. Hard-swapping to one would have quietly broken
    /// the other, and the shim workflow is documented in the Justfile.
    #[arg(long, value_parser = ["anthropic", "openai"], default_value = "anthropic")]
    provider: String,
    /// Per-branch HTTP timeout in seconds.
    ///
    /// NOT generous, which is what this said before it was measured — and the gate
    /// is not what eats it. Measured on the clinic: a gate run from a fresh
    /// candidate path against the shared cargo cache, including the recompile, the
    /// composition, booting a host and fifteen HTTP assertions, is 2.3 SECONDS.
    ///
    /// The budget goes to the model. From one real run's host log against the API,
    /// 11 completed calls: median 64s, mean 80s, slowest 174s. A branch makes up to
    /// `attempts` of those in sequence, so two from the slow tail plus the gate lands
    /// on 300s exactly — which is why some branches die and others do not.
    ///
    /// A LOCAL model moves the numbers by an order of magnitude, and the same
    /// arithmetic then argues for a much larger budget. Measured twice against
    /// Qwen3.8-27B-4bit on `csatapaci`, the mlx server `.comp/csatapaci.env` points at:
    ///
    ///     prompt 5266 tok, out 2048 tok    417s / 303s
    ///     prompt 5261 tok, out  138 tok     64s /  69s
    ///
    /// The first row is a branch's real shape — a contract and a base tree in, a module
    /// out — so two attempts is 600-834s, and `GOAL_TIMEOUT` is 1800 there rather than
    /// 900. Note which end is slow: 64s for 138 output tokens is almost all prefill, so
    /// a bigger CONTRACT costs more than a longer answer.
    ///
    /// What a branch over budget looks like is not a timeout message: the
    /// reconciler's client hangs up,
    /// the host logs `hyper::Error(IncompleteMessage)`, the ingress logs `connection
    /// closed before message completed`, and the run reports `error sending request
    /// for url .../run`. Three branches died that way in one clinic run and four in
    /// another, and every one of them read as a fleet fault rather than as this
    /// number being too small.
    ///
    /// 900 leaves ~2.5x headroom over the slowest pair observed. That, not a bigger
    /// machine, is the fix — and it is now the default, because it was written here
    /// as the answer and left at 300. A `card-identify` run through the Claude CLI
    /// shim, where calls are slower still (90-135s each, vs the 64s median above),
    /// lost branch-0 to this in BOTH generations: two of six branch-runs, a third
    /// of the budget, zero attempts made, reported as `error sending request`.
    #[arg(long, default_value_t = 900)]
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
pub struct GoalSpec {
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
    /// A workspace manifest in the base tree whose `members` is trimmed to `keep_members` before the gate
    /// sees it, so one crate of a shared workspace builds standalone while its `.workspace = true`
    /// inheritance still resolves.
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
    /// Files a branch is SHOWN but may not write — its held-out tests, most
    /// usefully.
    /// Without it a branch is told "a held-out spec judges you" and handed only the filename; showing the
    /// file up front costs one prompt instead of one generation.
    ///
    /// Not writable, and not enforced here — `writable` is the allow-list the
    /// applier checks, so naming a file here grants no write.
    #[serde(default)]
    context: Vec<String>,
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
    /// usefully. Without them a part is told "your tests judge you" and handed none, so it writes blind.
    #[serde(default)]
    context: Vec<String>,
    /// This half's own gate. It runs against the contract alone — the frontend
    /// against fixtures generated from it, the backend against the routes it
    /// promises — so neither part ever waits for the other.
    #[serde(rename = "check")]
    checks: Vec<CheckSpec>,
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
    /// Check ids that must PASS before this one runs.
    /// Absent means no edges: a one-level graph. A candidate that does not compile comes back as one
    /// failure plus a list of checks nobody tried, not every check failing at once (ADR-0088).
    #[serde(default)]
    needs: Vec<String>,
}
fn yes() -> bool {
    true
}
fn one() -> u32 {
    1
}

/// Returns the exit code as a VALUE rather than calling `std::process::exit`: `main` is the only
/// caller, after this returns, so `Checks`/`Fleet` drop-clean. `exit` skips destructors and used to
/// leak the whole internal fleet on EXHAUSTED or GATE_REFUSED runs.
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

    let mut tree = base_tree(&args.checkout, &goal.base_paths)?;
    // Trim a shared workspace manifest to the goal's target member, so one crate
    // of a big workspace builds standalone in the gate.
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

    // The commands the gate is allowed to run: the first word of each check.
    // Deduped, so `--allow uv` appears once however many checks use it.
    //
    // EVERY check, including the parts': a decomposed goal's top-level list is only the composition gate,
    // and deriving from it refused every part's own command.
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
    // Said differently when the runner is not ours: `allow` only reaches a runner THIS process starts; one
    // already listening has its own `--allow`, which this list does not affect.
    match &args.checks_url {
        None => println!("gate allows: {allow:?}"),
        Some(u) => println!(
            "gate needs: {allow:?} — and this run does not set that: {u} was started by \
             somebody else, with its own --allow"
        ),
    }
    // A branch makes up to `attempts` model calls IN SEQUENCE, so the per-branch timeout must hold all of
    // them, or it dies with `error sending request` and zero attempts, which reads as a fleet fault.
    // Said out loud rather than corrected: call speed depends on the provider and a silently raised
    // timeout is its own surprise. 150s per call is the slow tail of the Claude CLI shim.
    let needed = args.attempts as u64 * 150 + 30;
    if args.timeout < needed {
        println!(
            "WARNING: --timeout {}s cannot hold {} sequential model calls (~{}s needed).\n                      A branch that runs out dies with `error sending request` and NO attempts.\n                      Raise --timeout or lower --attempts.",
            args.timeout, args.attempts, needed
        );
    }
    // Said out loud: a gate that cannot find the host reports the same as a candidate that does not work.
    let hb = host_bin();
    println!(
        "gate host:   {} ({})",
        hb.display(),
        if hb.exists() { "found" } else { "MISSING — every check that runs the app will fail" }
    );

    // A warm, SHARED tool cache for the gate, and the environment that points
    // every check at it. See `setup::warm_caches`.
    let check_env = setup::warm_caches(&goal, &args);

    // Bring the gate up first, so the driver fixture can point at it — or find
    // out where the one somebody else is running lives.
    let gate = Gate::open(
        args.checks_url.as_deref(),
        args.checks_token_file.as_deref(),
        args.check_timeout,
        &allow,
        &check_env,
    )?;

    set_fleet_timeouts(&args);
    // The fixtures the fleet is started from, the secrets granted to them, and
    // the artifacts they name. See `setup::render_specs`.
    let setup::Deployment { specs, secrets, artifacts: art } =
        setup::render_specs(&args, &goal, &gate)?;
    let spec_refs: Vec<&str> = specs.iter().map(String::as_str).collect();

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
                // Read here rather than reusing the binding below (declared after this block); reading the file twice
                // costs nothing.
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

    // Seed the runner so nothing downstream carries the tree: once it is keyed by commit, a branch's plan
    // names the commit only, keeping 500 KB of repository off the lattice per generation. Must happen
    // BEFORE the critic, which may fail.
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
        // EXIT 4, not 0: a gate that could not judge spent NOTHING, and success made that look like a
        // candidate passing (the daemon read it as `awaiting-human`). Distinct from `goalexit::EXHAUSTED`
        // (3), where every branch ran and none passed.
        return Ok(goalexit::GATE_REFUSED);
    }

    if args.smoke {
        return smoke(&args, &goal, port, &context, &checks, &base_commit, &allow)
            .map(|()| goalexit::SUCCESS);
    }

    // Has this already been done?
    if !pool::worth_running(memory.as_ref(), &goal, args.skip_above) {
        return Ok(goalexit::SUCCESS);
    }

    // What this run leaves behind (ADR-0092). BEFORE the decomposed dispatch: a two-part run once returned
    // before any `Trace` existed and recorded nothing, silently. The seed IS the run id;
    // `run_id(seed, round, branch)` is one attempt inside it, so a run has ONE identity.
    let seed = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_secs();
    let run = seed.to_string();
    // The password by VALUE: `--surreal-password` is a path so it stays out of argv, but the trace talks
    // to the database directly. Absent means unauthenticated.
    let surreal_password = args
        .surreal_password
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string());
    // `None` without `--surreal-url`, like the pool: a run with no database is supported (ADR-0080).
    let trace = args.surreal_url.as_deref().map(|url| {
        // The same database the graph component was pointed at, so a run and the
        // lessons it produced land in one place and can be joined (ADR-0091).
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

    // What the pool already has (ADR-0094), ABOVE the decomposed dispatch like the `Trace`: below it, a
    // two-part run asked the catalogue nothing, and the per-PART question is where "does this already
    // exist" is most likely yes.
    let (reuse, capability_confirmed) = pool::search_the_pool(
        &goal.text,
        &run,
        trace.as_ref(),
        args.capability_advisor_url.as_deref(),
    );

    // A DECOMPOSED goal: parts that compose rather than compete. Each half runs its own generations
    // against a shared contract and the winners merge into one tree judged by the goal's checks
    // (ADR-0086).
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
        )
        .map(|()| goalexit::SUCCESS);
    }

    println!("fleet serving; running the search …\n");

    let plan = json!({
        "text": goal.text,
        "writable": goal.writable,
        "context": context,
        "previous": [],
        "checks": checks,
        "base_commit": base_commit,
        // NOT the tree: the runner was seeded above and keys its cache by this commit, so carrying bytes
        // would cost 500 KB over two wrpc hops per branch per generation. `agent-driver` starts from
        // `base_known`; `need-base` remains an error, reached only if something cleared that cache.
        "base_tree": [],
        "max_attempts": args.attempts,
        "seed": 1,
    });

    // What each branch is allowed to read.
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

    // Every attempt of every branch, so the run is legible even when it fails.
    let mut entries: Vec<Entry> = Vec::new();
    let mut recorded = 0usize;
    // The accepted branch with the highest score as (run id, score), the run the PR is attributed to;
    // `search` picks the winning ENTRY but only this walk knows the run id.
    let mut winner: Option<(String, u64)> = None;
    for (r, round) in found.rounds.iter().enumerate() {
        for e in &round.entries {
            // One verdict per BRANCH, not per generation: the count of failed attempts on a goal says whether
            // another generation is worth buying. The artifact is empty here; the landing path re-reports the
            // winner with it (a verdict edge is keyed by (goal, run)).
            let attempt = run_id(seed, r, &e.branch);
            if e.accepted && winner.as_ref().is_none_or(|(_, best)| e.score > *best) {
                winner = Some((attempt.clone(), e.score));
            }
            if let Some(t) = &trace {
                // Spawned and finished are recorded together because this walk runs after the search; live progress
                // is the socket's job.
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
                    // How many tries this branch took: the one number that says whether repair earns its budget.
                    e.attempts,
                );
            }
            if let Some(m) = &memory {
                match m.evaluated(&goal.text, &attempt, e.score, e.accepted, "") {
                    Ok(()) => recorded += 1,
                    Err(err) => println!("  (verdict for {attempt} not recorded: {err})"),
                }
                // What this branch LEARNED by failing, in the gate's own words. No model in the path, so it cannot be
                // a hallucination, and its worst case is avoiding something that would have worked (ADR-0081's
                // asymmetry).
                if !e.accepted {
                    if let Some(text) = memory::failure_text(&e.failures, e.score) {
                        match m.observe_failure(&goal.text, &e.branch, &attempt, &text) {
                            Ok(h) => println!("  {} wrote a lesson: {h}", e.branch),
                            Err(err) => println!("  (lesson from {run} not recorded: {err})"),
                        }
                    }
                }
                // What happened to what this branch READ: the only thing that moves a lesson's standing.
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

    // Promote what the gate proved.
    pool::promote_and_sweep(
        memory.as_ref(),
        &args,
        &goal,
        port,
        found.best.as_ref(),
        &winner.as_ref().map(|(r, _)| r.clone()).unwrap_or_default(),
    );
    // What the pool gained (ADR-0089). Derived from the WINNER's paths, because a
    // capability the swarm can reuse is one that passed a gate — a component from
    // a branch that failed is a directory, not a capability.
    if let (Some(t), Some(best)) = (&trace, found.best.as_ref()) {
        if best.accepted {
            for (name, path) in new_capabilities(&best.files) {
                t.capability_added(&run, &name, &path);
            }
        }
    }

    if !found.accepted {
        let best = found.best.as_ref().map(|e| e.score).unwrap_or(0);
        println!("\nNothing passed the gate (best score {best}). No PR opened.");
        if let Some(b) = &found.best {
            println!("closest failing checks: {}", b.failures);
        }
        // `exhausted`, not `failed`: every branch ran and none passed; the count on a goal says whether
        // another generation is worth buying.
        if let Some(t) = &trace {
            t.run_resolved(&run, "exhausted", None, "");
            if let Some(why) = t.report() {
                println!("trace: {why}");
            }
        }
        // EXIT 3, not 0: an exhausted search is a legitimate outcome, not a success, and `comp-goald` once
        // marked it `awaiting-human`. 3 rather than 1 so it stays distinguishable from a run that BROKE;
        // a caller that wants to retry cares which.
        return Ok(goalexit::EXHAUSTED);
    }

    if args.dry_run {
        let best = found.best.as_ref().unwrap();
        println!("\n[dry run] a candidate passed (score {}); not opening a PR.", best.score);
        // And it is WRITTEN OUT: a dry run that discards the winner is a search paid for and kept nothing
        // from (a `card-identify` run once spent 42 calls and left the stub as it was). Into a directory,
        // not the checkout: a dry run must not mutate its tree.
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

    // Land the winner. A unique branch name per run, because a PR cannot reuse one.
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
    match land(&select_url, "goalland.acme.test", &entries, landing, timeout) {
        Ok(v) if v["url"].is_string() => {
            let url = v["url"].as_str().unwrap();
            println!("\n  PR opened: {url}");
            println!("  branch: {}  commit: {}", v["branch"], v["commit"]);
            // Re-report the winning run now that there is something addressable; idempotent per (goal, run).
            if let (Some(m), Some(w)) = (&memory, &winner) {
                if let Err(e) = m.evaluated(&goal.text, &w.0, w.1, true, url) {
                    println!("  (the pull request was not recorded against the goal: {e})");
                }
            }
            if let Some(t) = &trace {
                t.run_resolved(&run, "merged", winner.as_ref().map(|(w, _)| w.as_str()), url);
            }
        }
        Ok(v) => {
            println!("\n  the forge answered but opened no PR: {v}");
            // A branch passed the gate and the forge produced nothing: a FAILED run, not exhausted; conflating
            // them would hide the forge as a cause.
            if let Some(t) = &trace {
                t.run_resolved(&run, "failed", winner.as_ref().map(|(w, _)| w.as_str()), "");
            }
        }
        Err(e) => {
            println!("\n  landing failed: {e}");
            if let Some(t) = &trace {
                t.run_resolved(&run, "failed", winner.as_ref().map(|(w, _)| w.as_str()), "");
            }
        }
    }
    // One line, at the end, if anything did not land. Per-write reporting would
    // drown the run's real output on a database that is down.
    if let Some(why) = trace.as_ref().and_then(|t| t.report()) {
        println!("trace: {why}");
    }
    Ok(goalexit::SUCCESS)
}

/// Calls `std::process::exit` only after `run()` has returned and dropped its `Checks`, `Fleet` and
/// child processes; `exit` skips destructors, so never call it inside `run()`.
fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("Error: {e:?}");
            std::process::exit(1);
        }
    }
}

/// One goal, K parts, one pull request.
/// A generation with a brilliant backend and no frontend produced nothing: there is no "best" to land
/// until every part is green.
#[allow(clippy::too_many_arguments)]
fn decomposed(
    args: &Args,
    goal: &GoalSpec,
    port: u16,
    gate: &Gate,
    memory_for_parts: Option<Memory>,
    // Kept in the signature and unused on purpose: a part is shown its OWN files
    // plus what its `context` names, never the goal's top-level list — the bug
    // that had every part writing blind.
    _context: &[Value],
    tree: &[Value],
    base_commit: &str,
    composition_checks: &[Value],
    // The run's identity and its record, from the caller — see the ADR-0092 block
    // in `main` for why they are not constructed here.
    seed: u64,
    trace: Option<&Trace>,
    // What the catalogue answered about this goal, searched by the caller so one
    // run asks once and both paths ask at all (ADR-0094).
    reuse: &[comp_goal::capsearch::Capability],
    // Which of `reuse` a capability-advisor confirmed, if one was configured
    // and reachable — `None` means every hit is trusted unfiltered.
    capability_confirmed: Option<&std::collections::BTreeSet<String>>,
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
    // `publish` refuses a second contract so one cannot move what the parts build against mid-run; but a
    // repeat run (smoke then real) finds its own contract stored, and dying would make `--smoke`
    // once-only. So continue on what is stored and refuse only when it DIFFERS from the file.
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
    // What the parts build against is whatever is canonical NOW, which after an
    // earlier run's negotiation may be later than v1.
    let body = registry.get(version).ok().flatten().map(|c| c.body).unwrap_or(body);

    // Every part's context, not one branch's: a decomposed run has no single branch
    // to put this in, and a part that reimplements `auth-guard` fails ADR-0089's
    // gate whether or not anyone told it the component exists.
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
                // Its OWN files plus whatever it is shown and may not write, not the goal's top-level context
                // (usually empty for a decomposed goal, so a part would write blind). Writable files keep every
                // comment (the brief); read-only `.wit` context is trimmed by `lean_context`.
                "context": context,
                "previous": [],
                "checks": p.checks.iter().map(|c| json!({
                    "id": c.id, "required": c.required, "weight": c.weight, "command": c.command,
                    "needs": c.needs,
                })).collect::<Vec<_>>(),
                "base_commit": base_commit,
                // Empty as on the ordinary path, and it matters more here: the tree would cross the lattice K times.
                // `main` seeds once, before this is reached.
                "base_tree": [],
                "max_attempts": args.attempts,
                "seed": 1,
            }),
            }
        })
        .collect();
    println!("parts: {}", parts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", "));

    // The decomposition, into the pool: each part gets its own `task` row, and this edge records whose
    // part it is, so "what did this goal break into" is answerable and a sub-goal found later by
    // similarity is back in context. Written BEFORE the search, because a pool that only sees
    // successful decompositions has never seen a bad one. `done` derives from each part's verdict edges,
    // so this says nothing about outcomes. Reported, never fatal (ADR-0084's asymmetry).
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
    // `seed` is the caller's — one run, one identity, and the trace keys on it.
    let run_key = seed.to_string();
    // How the run ended, said once: every `return` below goes through it, so an early exit cannot leave
    // a run row that started and never resolved (indistinguishable from a crash).
    let resolve = |outcome: &str, url: &str| {
        if let Some(t) = trace {
            // "composition", not a branch name: the JOIN passed a gate neither half could pass alone; naming one
            // part's branch would credit half the work.
            let winner = (outcome == "merged").then_some("composition");
            t.run_resolved(&run_key, outcome, winner, url);
            if let Some(why) = t.report() {
                println!("\ntrace: {why}");
            }
        }
    };

    // The loop itself is library code, so the e2e that covers it drives THIS and
    // not a re-spelling of it. What is left here is what a binary is for: saying
    // what happened, and landing it.
    let run = compose::run_parts(
        &compose::Wiring {
            driver_url: &format!("http://127.0.0.1:{port}/run"),
            driver_host: "goalrun.acme.test",
            checks_url: &gate.url(),
            checks_token: gate.token().as_deref(),
            registry: &registry,
            answerer: Some(&answerer),
            // A decomposed run reads, writes, attributes and forgets exactly as an
            // ordinary one does — per PART, on that part's own goal.
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

    // The record, before anything below can fail: every branch of every generation of every part. The
    // part name is IN the attempt id, since two parts each run `branch-0` in round 0 and
    // `run_id(seed, round, branch)` alone would overwrite one half's history.
    if let Some(t) = trace {
        for p in &run.composition.parts {
            for (r, round) in p.rounds.iter().enumerate() {
                for e in &round.entries {
                    let attempt = run_id(seed, r, &format!("{}/{}", p.part, e.branch));
                    t.branch_spawned(&run_key, &attempt, &e.branch, r);
                    t.gate_verdict(&run_key, &attempt, e.score, e.accepted, &e.failures);
                    // `errored` when the branch produced nothing (a provider failure or an answer with no file block);
                    // "failed" is for candidates the gate judged.
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
        // Why a branch produced NOTHING, a different question from why a candidate failed: the note is the
        // only place a transport failure, refused plan or dead provider says so.
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
        // Which check, and what it said: without it "component never passed its gate" is the least
        // actionable way to end.
        for p in &run.composition.parts {
            if let Some(best) = p.best.as_ref().filter(|_| !p.accepted) {
                if !best.failures.as_array().map(|a| a.is_empty()).unwrap_or(true) {
                    println!("\n  {} was still failing:", p.part);
                    for f in best.failures.as_array().unwrap() {
                        println!("    · {}: {}", f["id"].as_str().unwrap_or("?"), {
                            // The LAST lines, not the first: a failing command
                            // says what went wrong at the end and spends its
                            // beginning telling you what it is doing.
                            let d = f["detail"].as_str().unwrap_or("");
                            let lines: Vec<&str> =
                                d.lines().filter(|l| !l.trim().is_empty()).collect();
                            lines[lines.len().saturating_sub(6)..].join("\n      ")
                        });
                    }
                }
            }
        }
        // WHO a join failure is about: the halves each passed and the whole did not, so no part's own gate
        // says anything.
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

        // And the merged tree is WRITTEN OUT: a join failure otherwise discards every part's accepted code,
        // the most expensive discard in the loop.
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
        return Ok(());
    }
    let report = run.report.as_ref().expect("landable means the gate ran");
    let changes = run.changes.clone().expect("landable means there is a tree");
    println!("  composition PASSED at score {}", report.score);

    // The gate accepted, so the graph may learn from success, before the PR: promotion is earned by the
    // verdict, not by the forge accepting the branch.
    pool::promote_parts(memory_for_parts.as_ref(), goal, port, &run.composition.parts);
    // And what the pool GAINED, from the merged tree: the join is what passed (ADR-0089). Same derivation
    // as the ordinary path.
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
        return Ok(());
    }

    // One pull request, carrying every part's work and the negotiation that got
    // them there — the part a reviewer most needs and could never reconstruct.
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
            let url = v["url"].as_str().unwrap();
            println!("\n  PR opened: {url}");
            println!("  branch: {}  commit: {}", v["branch"], v["commit"]);
            resolve("merged", url);
        }
        Ok(v) => {
            println!("\n  the forge answered but opened no PR: {v}");
            resolve("failed", "");
        }
        Err(e) => {
            println!("\n  landing failed: {e}");
            resolve("failed", "");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::GoalSpec;
    use comp_goal::gate::egress_authority;

    /// The goal spec for a decomposed run, as a person writes it.
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

    /// The egress allow-list is derived from the base URL, so a mismatch is
    /// impossible by construction — but only if this derivation is right. A wrong
    /// answer fails at the first inference call with an egress error naming a host
    /// nobody typed, which is a bad place to start debugging.
    #[test]
    fn the_egress_authority_comes_from_the_base_url() {
        assert_eq!(egress_authority("https://api.anthropic.com"), "api.anthropic.com");
        // The shim. The port is KEPT: a bare `127.0.0.1` entry would also work,
        // and would allow every port on loopback rather than this one.
        assert_eq!(egress_authority("http://127.0.0.1:8787"), "127.0.0.1:8787");
        assert_eq!(egress_authority("http://localhost:8787/v1"), "localhost:8787");
        // A path must not leak into the authority.
        assert_eq!(egress_authority("https://proxy.internal/anthropic"), "proxy.internal");
        assert_eq!(egress_authority("http://[::1]:8787"), "[::1]:8787");
    }

    /// Two parts running the same branch of the same round are two attempts.
    /// A decomposed run breaks the per-run uniqueness of `run_id(seed, round, branch)`: both halves spawn
    /// `branch-0` in round 0, so the second would silently overwrite the first's attempt row.
    #[test]
    fn two_parts_do_not_share_one_attempt_id() {
        use comp_goal::memory::run_id;
        assert_ne!(
            run_id(7, 0, "access-and-search/branch-0"),
            run_id(7, 0, "reports/branch-0"),
            "the part name must be part of the key"
        );
        // And a run still separates its own rounds and branches.
        assert_ne!(run_id(7, 0, "reports/branch-0"), run_id(7, 1, "reports/branch-0"));
        assert_ne!(run_id(7, 0, "reports/branch-0"), run_id(7, 0, "reports/branch-1"));
    }
}
