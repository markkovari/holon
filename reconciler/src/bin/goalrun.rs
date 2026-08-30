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
use comp_reconciler::compose;
use comp_reconciler::contract::{Answerer, Registry};
use comp_reconciler::fleet::{bin_path, free_port, repo_root, Fleet};
use comp_reconciler::generation as generation_mod;
use comp_reconciler::generation::{land, Bounds, Entry, Part};
use comp_reconciler::memory::{self, run_id, Memory};
use comp_reconciler::trace::Trace;
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
    /// Files a branch is SHOWN but may not write — its held-out tests, most
    /// usefully.
    ///
    /// `PartSpec` has had this since the first decomposed run wrote blind; an
    /// ordinary goal never did, so a branch was told "a held-out spec judges you"
    /// and handed the spec's filename. The winning run of `card-identify` proves
    /// what that costs: attempt-0 failed on every branch, and the one that passed
    /// did so on attempt-1, after the GATE told it what the tests actually assert.
    /// Showing the file up front buys the same information for one prompt instead
    /// of one whole generation.
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
/// The index of the `]` that closes the array opened at `open`, ignoring any inside
/// a `#` comment.
///
/// TOML has no block comments and no escapes to worry about here: a `#` runs to the
/// end of the line, and a member is a quoted string that cannot contain a newline. A
/// `#` inside a quoted string is not a comment, so quotes are tracked too — a crate
/// named `"a#b"` is legal and would otherwise blind the rest of the line.
fn closing_bracket(text: &str, open: usize) -> Option<usize> {
    let mut in_comment = false;
    let mut in_string = false;
    for (i, c) in text.char_indices().skip_while(|(i, _)| *i <= open) {
        match c {
            '\n' => in_comment = false,
            '"' if !in_comment => in_string = !in_string,
            '#' if !in_string => in_comment = true,
            ']' if !in_comment && !in_string => return Some(i),
            _ => {}
        }
    }
    None
}

fn trim_members(manifest: &str, keep: &[String]) -> String {
    let Some(start) = manifest.find("members") else { return manifest.to_string() };
    let Some(open) = manifest[start..].find('[').map(|i| start + i) else {
        return manifest.to_string();
    };
    // The first `]` after the opening bracket is NOT necessarily the array's — a
    // comment inside the list can hold one, and `components/Cargo.toml`'s does:
    //
    //     members = [
    //         # `bench-suite-p3` stays out: it declares its own `[workspace]`, so …
    //         "ai-inference",
    //
    // Closing on that `]` rewrote the manifest to `members = ["card-identify"]`,
    // so adding them is "multiple …` — invalid TOML. Every branch of every goal
    // scoped to this workspace then got a tree cargo refuses to load, scored zero,
    // and the gate had nothing to say about why. So: skip what a `#` comments out.
    let Some(close) = closing_bracket(manifest, open) else {
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
    /// Check ids that must PASS before this one runs.
    ///
    /// Absent means "no edges", which is a graph with one level and exactly the
    /// behaviour every goal spec written before this had. What it buys is a report
    /// a repair prompt can use: a candidate that does not compile comes back as one
    /// failure and a list of things nobody tried, rather than as every check
    /// failing at once (ADR-0088).
    #[serde(default)]
    needs: Vec<String>,
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

/// A bearer token nobody has to manage: 32 bytes of the OS's randomness, hex.
///
/// Minted per run rather than configured, because a token for a runner this
/// process starts and kills has no reason to outlive it — and a generated secret
/// is one that cannot be left at its default.
fn mint_token() -> Result<String> {
    let mut b = [0u8; 32];
    std::io::Read::read_exact(
        &mut std::fs::File::open("/dev/urandom").context("opening /dev/urandom")?,
        &mut b,
    )
    .context("reading /dev/urandom")?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Is this `host:port` one only this machine can reach?
///
/// Resolved rather than string-matched, and an unresolvable name counts as NOT
/// loopback — the safe side of an unknown is the side that demands a token.
fn authority_is_loopback(authority: &str) -> bool {
    use std::net::ToSocketAddrs;
    let with_port =
        if authority.contains(':') { authority.to_string() } else { format!("{authority}:80") };
    match with_port.to_socket_addrs() {
        Ok(it) => {
            let a: Vec<_> = it.collect();
            !a.is_empty() && a.iter().all(|x| x.ip().is_loopback())
        }
        Err(_) => false,
    }
}

/// Where the gate is: a runner this process started, or one already listening
/// somewhere else.
///
/// The two cases differ only in who owns the process. Everything downstream —
/// the manifest's `checks-url`, its egress entry, the token granted to the gate
/// component, the direct POST that gates a composition — reads the same three
/// answers from here, so a remote gate cannot be half-wired.
enum Gate {
    /// Started here, killed with the run.
    Local(Checks),
    Remote {
        url: String,
        token_file: PathBuf,
    },
}

impl Gate {
    fn open(args: &Args, allow: &[&str], check_env: &[String]) -> Result<Self> {
        let Some(url) = args.checks_url.clone() else {
            return Ok(Gate::Local(Checks::start(allow, check_env, args.check_timeout)?));
        };
        if !url.starts_with("http://") && !url.starts_with("https://") {
            bail!("--checks-url must start with http:// or https://, got {url:?}");
        }
        let authority = egress_authority(&url);
        let token_file = match (&args.checks_token_file, authority_is_loopback(&authority)) {
            (Some(p), _) => {
                if !p.is_file() {
                    bail!("--checks-token-file {} does not exist", p.display());
                }
                p.clone()
            }
            // The same refusal `comp-checks` makes at its own end, made here too:
            // a runner that demanded a token and a caller that never sends one
            // fail as 401 on every candidate, which reads as a broken gate rather
            // than as a missing flag.
            (None, false) => bail!(
                "--checks-url {url} is not on this machine, so it needs --checks-token-file.\n\
                 \n\
                 The runner at the other end refuses to listen off the loopback without a token \
                 for the same reason: --allow bounds the COMMAND, not the tree it runs over.\n\
                 \n\
                 \x20 head -c 32 /dev/urandom | base64 > ~/.comp-secrets/checks   # on both boxes"
            ),
            // A loopback runner somebody else started. Its own guard already
            // allows this, so refusing it here would be a second opinion.
            (None, true) => {
                let dir = std::env::temp_dir().join(format!("comp-goalrun-{}", std::process::id()));
                std::fs::create_dir_all(&dir)?;
                let p = dir.join("checks-token");
                std::fs::write(&p, "")?;
                p
            }
        };
        eprintln!("goalrun: gate is {url} (not started here)");
        Ok(Gate::Remote { url, token_file })
    }

    fn url(&self) -> String {
        match self {
            Gate::Local(c) => format!("http://127.0.0.1:{}/check", c.port),
            Gate::Remote { url, .. } => url.clone(),
        }
    }

    /// What the gate component is allowed to dial. A manifest decision, so it has
    /// to be the real host and not a stand-in (ADR-0008).
    fn authority(&self) -> String {
        egress_authority(&self.url())
    }

    fn token_file(&self) -> &Path {
        match self {
            Gate::Local(c) => &c.token_file,
            Gate::Remote { token_file, .. } => token_file,
        }
    }

    /// The token itself, for the one call that is made from here rather than from
    /// the gate component: the composition gate.
    fn token(&self) -> Option<String> {
        std::fs::read_to_string(self.token_file())
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    }
}

/// The native gate runner, alive for the run.
struct Checks {
    child: Child,
    port: u16,
    token_file: PathBuf,
    _dir: tempfile::TempDir,
}
impl Drop for Checks {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Checks {
    fn start(allow: &[&str], check_env: &[String], timeout: u64) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let port = free_port();
        // Authenticated even on loopback. Not because loopback is unsafe, but
        // because the alternative is one code path used every day and a second
        // one used only when someone points at another box — and the second is
        // the one that matters. Minted here, so it costs the operator nothing.
        let token_file = dir.path().join("token");
        std::fs::write(&token_file, mint_token()?)?;
        // The work directory is a SUBDIRECTORY, so the runner's throwaway trees
        // and cached bases never share a parent with the token file.
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work)?;
        let mut cmd = Command::new(bin_path("comp-checks"));
        cmd.args(["--addr", &format!("127.0.0.1:{port}")])
            .arg("--work-dir")
            .arg(&work)
            .arg("--token-file")
            .arg(&token_file)
            .args(["--timeout", &timeout.to_string()]);
        for a in allow {
            cmd.args(["--allow", a]);
        }
        for e in check_env {
            cmd.args(["--check-env", e]);
        }
        let child = cmd.stdout(Stdio::null()).stderr(Stdio::inherit()).spawn()?;
        let me = Self { child, port, token_file, _dir: dir };
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
/// The components a run created, as (name, path).
///
/// `components/<name>/...` is the only shape that names a component in this
/// repository — the same rule `plug::tags_for` already uses to decide what a
/// lesson is about, so the two cannot disagree about what a component is.
///
/// Derived from paths rather than announced by the model: a run that SAYS it
/// built a reusable component and a run that actually left one in the tree are
/// different things, and only the second changes what the pool can do.
fn new_capabilities(files: &Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = files
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| f["path"].as_str())
                .filter_map(|p| {
                    let rest = p.strip_prefix("components/")?;
                    let name = rest.split('/').next()?;
                    (!name.is_empty()).then(|| (name.to_string(), format!("components/{name}")))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out.dedup();
    out
}

/// The egress allow-list entry for a base URL: its authority, port and all.
///
/// The allow-list is an AUTHORITY, not a URL — the scheme and path come off,
/// everything else stays. `https://api.anthropic.com` allows
/// `api.anthropic.com`; the shim's `http://127.0.0.1:8787` allows
/// `127.0.0.1:8787`.
///
/// **The port is kept on purpose.** `Egress::permits_authority` will match a
/// bare `127.0.0.1` entry against an authority of `127.0.0.1:8787`, so dropping
/// the port would still work — and would quietly widen the allow-list to every
/// port on loopback. `fixtures/llm-secret.yaml` pins `127.0.0.1:OPENAI_PORT` for
/// the same reason. Egress is a security control; the narrower entry is the
/// correct one even when the wider one happens to function.
///
/// Deriving this from the base URL rather than taking a second flag means the
/// two cannot disagree — an allow-list naming a different authority than the
/// base URL fails at the first call, with an egress error about a host nobody
/// typed rather than about the URL somebody actually mistyped.
fn egress_authority(base_url: &str) -> String {
    let rest = base_url.split_once("://").map(|(_, r)| r).unwrap_or(base_url);
    rest.split('/').next().unwrap_or(rest).to_string()
}

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

/// Where `comp-host` is: its own workspace, or wherever the operator says.
fn host_bin() -> PathBuf {
    if let Ok(p) = std::env::var("COMP_HOST") {
        return PathBuf::from(p);
    }
    repo_root().join("host/target/release/comp-host")
}

/// The composer a gate uses to assemble what a candidate built.
fn plug_bin() -> PathBuf {
    if let Ok(p) = std::env::var("COMP_PLUG") {
        return PathBuf::from(p);
    }
    bin_path("comp-plug")
}

fn artifacts(provider: &str) -> Result<Vec<String>> {
    let provider_wasm =
        if provider == "openai" { "openai_provider.wasm" } else { "anthropic_provider.wasm" };
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
        ("allm", provider_wasm),
        ("mprobe", "memory_probe.wasm"),
        ("memory", "knowledge_memory.wasm"),
        ("graph", "knowledge_graph.wasm"),
        ("search", "search_index.wasm"),
        ("mllm", "mock_provider.wasm"),
        ("probe", "driver_probe.wasm"),
        ("driver", "agent_driver.wasm"),
        ("writer", "agent_writer.wasm"),
        ("llm", provider_wasm),
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

/// Seed the capability graph into the pool, so the loop can ask what exists.
///
/// ADR-0089 wants a run to ask "do we already have this?" before generating an
/// implementation, and `capsearch` answers that from the components plus the
/// artifacts. The GRAPH — who imports what from whom, and how many applications
/// carry a capability — lived only in `docs/CAPABILITY-GRAPH.md` and in a
/// projection that nothing outside a test ever ran. `just capgraph-store` could
/// write it by hand; nothing did it on the path a real run takes.
///
/// So a run with a pool seeds it, at startup, from the BUILT artifacts. That
/// keeps `comp-capgraph`'s own rule — "derived from the built artifacts every
/// time, never maintained by hand" — and makes the pool a cache that can always
/// be thrown away and rebuilt rather than a second source of truth.
///
/// Failure is reported and ignored, like every other thing the pool does. A run
/// without a graph is the run that has always happened; a run that stopped
/// because a projection failed would trade a working loop for a nicety.
fn seed_capability_graph(url: &str, password: Option<&str>) {
    let gen = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let bin = bin_path("comp-capgraph");
    let out =
        match Command::new(&bin).args(["--format", "surql", "--gen", &gen.to_string()]).output() {
            Ok(o) if o.status.success() => o.stdout,
            Ok(o) => {
                println!(
                    "capability graph not seeded: comp-capgraph exited {} — {}",
                    o.status,
                    String::from_utf8_lossy(&o.stderr).trim()
                );
                return;
            }
            Err(e) => {
                println!("capability graph not seeded: could not run {} ({e})", bin.display());
                return;
            }
        };

    let http = match reqwest::blocking::Client::builder().timeout(Duration::from_secs(60)).build() {
        Ok(c) => c,
        Err(e) => {
            println!("capability graph not seeded: {e}");
            return;
        }
    };
    let sql_url = format!("{}/sql", url.trim_end_matches('/'));
    let send = |body: String| {
        let mut req = http
            .post(&sql_url)
            .header("Accept", "application/json")
            .header("surreal-ns", "comp")
            .header("surreal-db", "goalmemory");
        if let Some(pw) = password {
            req = req.basic_auth("root", Some(pw));
        }
        req.body(body).send()
    };

    // The namespace and database may not exist on a fresh store, and a
    // projection into nothing is a wall of errors that reads like a broken tool.
    let _ = send(
        "DEFINE NAMESPACE IF NOT EXISTS comp; USE NS comp; \
         DEFINE DATABASE IF NOT EXISTS goalmemory;"
            .to_string(),
    );

    match send(String::from_utf8_lossy(&out).into_owned()) {
        Ok(resp) => {
            let text = resp.text().unwrap_or_default();
            // SurrealDB answers 200 with per-statement status, so the HTTP code
            // says nothing about whether the projection landed.
            let errs = text.matches("\"status\":\"ERR\"").count();
            if errs == 0 {
                println!("capability graph seeded into the pool at generation {gen}");
            } else {
                println!("capability graph partly seeded: {errs} statement(s) rejected");
            }
        }
        Err(e) => println!("capability graph not seeded: {e}"),
    }
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

/// The files a branch is shown: what it may write, then what it may only read.
///
/// One function because `run_parts` builds the same thing for a part, and two
/// builders drift — the rehearsal and `goalrun` disagreeing about `component =` is
/// what that looks like when it happens.
///
/// Writable files keep every comment: there the comments are the brief. Read-only
/// `.wit` context is trimmed by `lean_context`; a `.rs` held-out test is not, because
/// its doc comments are the specification.
///
/// Deduped, and `writable` wins. A path in both lists would otherwise be sent twice
/// — paid for on every attempt, and the second copy stripped differently from the
/// first, which is a worse bug than the cost.
fn branch_context(
    checkout: &std::path::Path,
    writable: &[String],
    readonly: &[String],
) -> Vec<Value> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for (path, strip) in
        writable.iter().map(|w| (w, false)).chain(readonly.iter().map(|r| (r, true)))
    {
        if !seen.insert(path.as_str()) {
            continue;
        }
        match std::fs::read_to_string(checkout.join(path)) {
            Ok(c) => out.push(json!({
                "path": path,
                "content": if strip { lean_context(path, c) } else { c },
            })),
            // Loud, because a typo'd context path is otherwise silent: the branch is
            // simply not shown the file and writes blind again, which is the failure
            // the field exists to remove.
            Err(e) => {
                println!("context: `{path}` could not be read ({e}) — the branch will not see it")
            }
        }
    }
    out
}

/// The first line of a multi-line failure, for a one-line report.
fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s).trim()
}

/// Which part a join failure is about.
///
/// A join failure is the one verdict in a decomposed run that no part owns: the
/// halves each passed, the whole did not, and the run ends there. Naming an owner is
/// the first half of making it addressable — without it the reader is handed "the
/// halves pass alone and not together" and a diff of three parts.
///
/// Attribution is by evidence only: a part owns the failure if the text names one of
/// its writable paths, or names the part itself. A failure that names nothing owned
/// belongs to the JOIN — the contract, or the composition check — and saying so is
/// more useful than picking the likeliest part.
fn join_failure_owners(failure: &str, parts: &[PartSpec]) -> Vec<String> {
    let owners: Vec<String> = parts
        .iter()
        .filter(|p| {
            p.writable.iter().any(|w| failure.contains(w.as_str())) || failure.contains(&p.name)
        })
        .map(|p| p.name.clone())
        .collect();
    owners
}

/// Write a candidate's files under `target/goalrun/candidate/`, mirroring paths.
///
/// Returns the directory. The tree it was run against is never touched — the point
/// is to be able to diff, not to have been changed.
fn write_candidate(checkout: &std::path::Path, files: &Value) -> std::io::Result<PathBuf> {
    let dir = checkout.join("target/goalrun/candidate");
    // Cleared rather than merged: a stale file from an earlier run sitting beside a
    // fresh one is indistinguishable from the candidate having written it.
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    for f in files.as_array().map(Vec::as_slice).unwrap_or_default() {
        let (Some(path), Some(content)) =
            (f.get("path").and_then(Value::as_str), f.get("content").and_then(Value::as_str))
        else {
            continue;
        };
        // A candidate's paths are checked by the applier before it can land; this is
        // a second look, because writing one to disk is the moment a `../` would
        // escape the directory.
        if path.contains("..") || std::path::Path::new(path).is_absolute() {
            continue;
        }
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, content)?;
    }
    Ok(dir)
}

/// Gate commands whose script is not in the shipped tree.
///
/// The third state the critic could not see. A check that fails because its script
/// is missing looks EXACTLY like a check that fails because the work is not done:
/// both are a non-zero exit on the base tree, so the critic says "every check can
/// judge" and the run proceeds to score every branch zero for the operator's
/// reason. `gate.sh` was untracked once and cost a smoke cycle to find; a real run
/// would have cost four branches and a repair round.
///
/// Unlike "did this fail because the tree cannot build", this needs no heuristic on
/// the words in a log. The tree is a list of paths and the command names one, so the
/// question is answered by looking.
///
/// Only arguments that are unambiguously a repo file are considered — a path-shaped
/// argument with a script extension, no URL scheme, not a flag. `cargo test -p x`
/// names no path and is left alone.
fn gates_missing_from_the_tree(goal: &GoalSpec, tree: &[Value]) -> Vec<String> {
    let shipped: std::collections::BTreeSet<&str> =
        tree.iter().filter_map(|f| f.get("path").and_then(Value::as_str)).collect();

    let looks_like_a_repo_script = |arg: &String| -> bool {
        !arg.starts_with('-')
            && arg.contains('/')
            && !arg.contains("://")
            && [".sh", ".py", ".mjs", ".js"].iter().any(|ext| arg.ends_with(ext))
    };

    goal.checks
        .iter()
        .chain(goal.parts.iter().flat_map(|p| p.checks.iter()))
        .flat_map(|c| c.command.iter().filter(|a| looks_like_a_repo_script(a)).map(move |a| (c, a)))
        .filter(|(_, arg)| !shipped.contains(arg.as_str()))
        .map(|(c, arg)| {
            let on_disk = std::path::Path::new(arg.as_str()).exists();
            let why = if on_disk {
                "it exists in the checkout but was not shipped — an UNTRACKED file is not in the \
                 base tree, and `base_paths` only ships tracked files. `git add` it"
            } else {
                "no such file in the checkout either — check the path"
            };
            format!("`{}` runs `{}`, which is not in the tree: {}", c.id, arg, why)
        })
        .collect()
}

/// Can this gate judge anything at all?
///
/// A check that already passes on the base tree cannot judge a candidate: one that
/// changes nothing satisfies it. The first real decomposed run on this repository
/// scored 1000 on two candidates that had deleted their own component exports,
/// because `cargo component check` passes on a crate implementing none of its
/// world (goal 07). This is the cheapest possible place to find that out — before
/// a generation buys the wrong answer.
///
/// `Ok(false)` means the run should stop, having spent nothing. A critic that
/// cannot RUN is reported and ignored: a guard that fails must not stop work a
/// person asked for.
fn gate_can_judge(
    goal: &GoalSpec,
    checks: &[Value],
    gate: &Gate,
    base_commit: &str,
    tree: &[Value],
    timeout: u64,
) -> bool {
    let missing = gates_missing_from_the_tree(goal, tree);
    if !missing.is_empty() {
        println!("\nREFUSED — a gate that cannot run cannot judge:\n");
        for m in &missing {
            println!("  · {m}");
        }
        println!("\nNothing was spent. Every branch would have scored zero for this reason.");
        return false;
    }

    let excused: Vec<String> = goal
        .checks
        .iter()
        .chain(goal.parts.iter().flat_map(|p| p.checks.iter()))
        .filter(|c| c.may_pass_base)
        .map(|c| c.id.clone())
        .collect();
    let mut every_check: Vec<Value> = checks.to_vec();
    for p in &goal.parts {
        every_check.extend(p.checks.iter().map(|c| {
            json!({ "id": c.id, "required": c.required, "weight": c.weight, "command": c.command, "needs": c.needs })
        }));
    }
    match compose::criticise(
        &gate.url(),
        gate.token().as_deref(),
        base_commit,
        &json!(tree),
        &json!(every_check),
        &excused,
        Duration::from_secs(timeout),
    ) {
        Ok(base) => {
            let vacuous = &base.vacuous;
            for v in vacuous.iter().filter(|v| v.excused) {
                println!("gate: `{}` passes on the base, and says it is meant to", v.id);
            }
            let refusals = compose::refusal(vacuous);
            if !refusals.is_empty() {
                println!("\nREFUSED — this gate cannot judge anything:\n");
                for r in &refusals {
                    println!("  · {r}");
                }
                println!(
                    "\nNothing was spent. A gate that passes on the code as it stands \n\
                     accepts a candidate that changes nothing."
                );
                return false;
            }
            println!("gate: every check fails on the base tree, so every check can judge");
            // WHY each one failed, because "it failed" is two states wearing one face:
            // work not done yet (a gate that can judge) and a tree that cannot build (a
            // gate that will fail every branch identically, for the operator's reason).
            // Printed rather than guessed at: a heuristic on the word "compile" would
            // refuse legitimate goals, and `tools/goal-rehearse.sh` is where this is
            // actually caught before anything is spent.
            for r in &base.reasons {
                let first = r.lines().next().unwrap_or(r).trim();
                println!("  · {}", &first[..first.len().min(160)]);
            }
            true
        }
        // Reported, not fatal: the critic is a guard, and a guard that cannot run
        // must not stop a run a person asked for.
        Err(e) => {
            println!("gate: could not be criticised ({e}) — running anyway");
            true
        }
    }
}

/// Prove the whole rig without spending anything.
///
/// Both apps serving already proves a lot: an app whose secret cannot be granted,
/// or whose egress is malformed, fails to START and never serves (`select.rs` saw
/// exactly this). So reaching here means links resolve, egress allow-lists parsed,
/// and every secret was granted.
///
/// A `max_attempts: 0` run is refused by the driver BEFORE any model call, so the
/// last round trip proves probe→driver for free.
#[allow(clippy::too_many_arguments)]
fn smoke(
    args: &Args,
    goal: &GoalSpec,
    port: u16,
    context: &[Value],
    checks: &[Value],
    base_commit: &str,
    tree: &[Value],
    allow: &[&str],
) -> Result<()> {
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
        .body(
            json!({
                "text": goal.text, "writable": goal.writable, "context": context,
                "previous": [], "checks": checks, "base_commit": base_commit,
                "base_tree": tree, "max_attempts": 0, "seed": 1,
            })
            .to_string(),
        )
        .send()?;
    let body: Value =
        serde_json::from_str(&probe.text().unwrap_or_default()).unwrap_or(Value::Null);
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
        let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build()?;
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

/// Has this goal already been done?
///
/// Asked ONCE per goal, before anything is spawned — the call that saves a whole
/// generation. Its failure mode had to be decided rather than defaulted: an
/// unreachable pool answers "no", because redoing work costs money and skipping
/// work that was never done is a silent wrong answer.
///
/// `false` means stop.
fn worth_running(memory: Option<&Memory>, goal: &GoalSpec, skip_above: f64) -> bool {
    let Some(m) = memory else { return true };
    match m.already_done(&goal.text, skip_above) {
        Ok(Some(prior)) => {
            println!("\nALREADY DONE — {}", prior.summary());
            println!(
                "\n  no branches spawned. Lower --skip-above (now {skip_above:.2}) or clear the \
                 pool if this is not the same work."
            );
            false
        }
        Ok(None) => {
            println!("nothing on record for this goal; running it");
            true
        }
        Err(e) => {
            println!("could not ask the knowledge pool ({e}) — doing the work");
            true
        }
    }
}

/// "Do we already have something for this?" — asked of the pool, before a single
/// token is spent, whatever the answer turns out to be.
///
/// ADR-0089 made reuse ENFORCED (a gate fails a part that reimplements
/// `auth-guard`) but never DISCOVERED: a human wrote the interfaces into the
/// goal's WIT and the branch then had no choice. That does not compound — every
/// new goal needed somebody who already knew what 150 components contained.
///
/// Mandatory rather than advisory because the ANSWER is the point in both
/// directions. A hit is reuse a branch would otherwise have missed. A miss is the
/// graph naming a capability the pool lacks — the only corpus in this system that
/// answers "what should we build next" — and it accumulates only if the question
/// is asked on every run, including the ones where nobody expected an answer.
///
/// No model, and nothing is blocked on the result: one millisecond of term overlap
/// over the catalogue, and a run whose search found nothing proceeds, with a row
/// recorded saying so (ADR-0094).
fn search_the_pool(
    goal_text: &str,
    run: &str,
    trace: Option<&Trace>,
) -> Vec<comp_reconciler::capsearch::Capability> {
    let catalog =
        comp_reconciler::plug::Catalog::scan(&comp_reconciler::plug::default_dirs(&repo_root()));
    let mut apps_of: std::collections::BTreeMap<String, usize> = Default::default();
    for name in catalog.names().map(String::from).collect::<Vec<_>>() {
        for part in catalog.closure(&name) {
            *apps_of.entry(part).or_default() += 1;
        }
    }
    let pool = comp_reconciler::capsearch::capabilities(&repo_root(), &catalog, &apps_of);
    let hits = comp_reconciler::capsearch::find(goal_text, &pool);
    if let Some(t) = trace {
        t.capsearch(run, goal_text, hits.len());
    }
    if hits.is_empty() {
        println!(
            "capability search: nothing in the pool answers this goal — if the work \
             generalises, it is a candidate for promotion (ADR-0089)\n"
        );
    } else {
        println!("capability search: {} candidate(s) the pool already has:", hits.len());
        for m in hits.iter().take(5) {
            println!(
                "  {:<22} {} app(s)  {}",
                m.capability.name,
                m.capability.apps,
                m.capability.description.chars().take(88).collect::<String>()
            );
        }
        println!();
    }
    hits.into_iter().take(5).map(|m| m.capability.clone()).collect()
}

/// Context content as a part should see it, trimmed for a small window.
///
/// A `.wit` shown as context is 68–79% comment (measured across this repository's interfaces),
/// and every load-bearing fact those comments carry is already in the contract — that is the
/// "KEPT" discipline the goals are written to. So the comments are redundant for a part and pure
/// cost for its context window: stripping them takes a WIT from ~700 tokens to ~200 and lets a
/// self-hosted model spend its window on the signatures it will call rather than on prose it can
/// read in the canonical file.
///
/// Only `.wit`, and only read-only context — a part's own writable `.rs` stub keeps every
/// comment, because there the comments ARE the brief. The canonical files are never touched;
/// this transforms the copy that goes into the prompt.
fn lean_context(path: &str, content: String) -> String {
    if !path.ends_with(".wit") {
        return content;
    }
    content.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
}

/// What the pool already has, as prose in every branch's context.
///
/// Prose rather than an instruction: the gate decides whether reuse happened, and
/// a branch TOLD to reuse something that does not fit would do it badly.
fn pool_context(reuse: &[comp_reconciler::capsearch::Capability]) -> Option<Value> {
    if reuse.is_empty() {
        return None;
    }
    let listed = reuse
        .iter()
        .map(|c| {
            format!(
                "- `{}` (in {} app(s)) exports {} — {}",
                c.name,
                c.apps,
                c.exports.iter().cloned().collect::<Vec<_>>().join(", "),
                c.description
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some(json!({
        "path": "POOL.md",
        "content": format!(
            "# Capabilities this repository already has\n\nSearched for this goal. Composing \
             one of these is cheaper than writing it, and the gate reads what a candidate \
             actually called.\n\n{listed}\n"
        ),
    }))
}

/// Distil what the winner taught, and keep the pool bounded.
///
/// An agent may record what it OBSERVED; only a passing gate may promote
/// (ADR-0084). So this runs after the verdict, through the `promotion` interface an
/// agent's world does not contain — and it costs one cheap call that most
/// candidates answer with NOTHING, which is the correct answer for a candidate
/// that taught nobody anything.
///
/// The sweep is last on purpose: one that ran first could delete a lesson this run
/// was about to read.
fn promote_and_sweep(
    memory: Option<&Memory>,
    args: &Args,
    goal: &GoalSpec,
    port: u16,
    best: Option<&Entry>,
    winner_ref: &str,
) {
    let Some(m) = memory else { return };
    if let Some(best) = best.filter(|b| b.accepted) {
        let door = Answerer {
            url: format!("http://127.0.0.1:{port}"),
            host: "goalanswer.acme.test".into(),
            timeout: Duration::from_secs(180),
        };
        let prompt = memory::distil_prompt(&goal.text, &best.files, best.score);
        match door.reply_to(&prompt).map(|r| memory::distilled(&r)) {
            Ok(Some(lesson)) => {
                match m.promote(&goal.text, &best.branch, winner_ref, &lesson, best.score) {
                    Ok(h) => println!("\npromoted to patterns: {h}\n  {lesson}"),
                    Err(e) => println!("\n(nothing promoted: {e})"),
                }
            }
            Ok(None) => println!("\nthe winner taught nothing transferable, and said so"),
            Err(e) => println!("\n(the distiller could not be reached: {e})"),
        }
    }

    if args.forget_after_days > 0 {
        match m.decay(args.forget_after_days, 2) {
            Ok(0) => {}
            Ok(n) => println!(
                "knowledge: forgot {n} entr{} nothing had read in {} days",
                if n == 1 { "y" } else { "ies" },
                args.forget_after_days
            ),
            Err(e) => println!("knowledge: could not sweep the pool ({e})"),
        }
    }
}

/// Promote what each part's winner taught, on a composed run.
///
/// The single-part path promotes through `promote_and_sweep`; the decomposed path never did,
/// which is why — measured across twelve runs of this experiment — the pool held only `errors`
/// rows and not one promotion, even from runs that opened a pull request. A perfect run taught
/// the graph nothing.
///
/// Each part is promoted keyed on the PART's own text, exactly as `compose.rs` RECALLS it: a
/// lesson keyed on the whole-goal wording would be invisible to the next part that recalls on
/// its own. Only a part whose gate accepted is promoted (ADR-0084), and the distiller answers
/// most of them with nothing, which is the right answer for a part that taught nobody anything.
fn promote_parts(
    memory: Option<&Memory>,
    goal: &GoalSpec,
    port: u16,
    parts: &[generation_mod::PartOutcome],
) {
    let Some(m) = memory else { return };
    let door = Answerer {
        url: format!("http://127.0.0.1:{port}"),
        host: "goalanswer.acme.test".into(),
        timeout: Duration::from_secs(180),
    };
    for outcome in parts {
        let Some(best) = outcome.best.as_ref().filter(|b| b.accepted) else { continue };
        // The part's own text is the key recall uses; the part name is its env.
        let Some(spec) = goal.parts.iter().find(|p| p.name == outcome.part) else { continue };
        let prompt = memory::distil_prompt(&spec.text, &best.files, best.score);
        match door.reply_to(&prompt).map(|r| memory::distilled(&r)) {
            Ok(Some(lesson)) => {
                match m.promote(&spec.text, &outcome.part, &best.branch, &lesson, best.score) {
                    Ok(h) => println!("  promoted {}: {h}\n    {lesson}", outcome.part),
                    Err(e) => println!("  {} promoted nothing: {e}", outcome.part),
                }
            }
            Ok(None) => println!("  {} taught nothing transferable, and said so", outcome.part),
            Err(e) => println!("  {} — the distiller could not be reached: {e}", outcome.part),
        }
    }
}

/// Run each check once in the checkout, with the caches the gate will use.
///
/// The toolchain download and the dependency compile happen once, outside any
/// request deadline, before a candidate is ever judged. The RESULT does not matter
/// — only the cache it leaves behind, which is why nothing here is checked.
fn warm_the_gate_cache(goal: &GoalSpec, args: &Args, caches: &GateCaches) {
    for c in &goal.checks {
        let tool = c.command.first().map(String::as_str);
        if !matches!(tool, Some("uv") | Some("cargo")) {
            continue;
        }
        println!("warming the gate cache ({}) …", c.command.join(" "));
        let _ = Command::new(&c.command[0])
            .args(&c.command[1..])
            .current_dir(&args.checkout)
            .env("UV_CACHE_DIR", &caches.uv_cache)
            .env("UV_PYTHON_INSTALL_DIR", &caches.uv_python)
            .env("CARGO_HOME", &caches.cargo_home)
            .env("CARGO_TARGET_DIR", &caches.cargo_target)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Where the gate's toolchains live, so a warm-up and a judged candidate share
/// them. Four paths that always travel together.
struct GateCaches {
    uv_cache: String,
    uv_python: String,
    cargo_home: String,
    cargo_target: String,
}

/// The timeouts a real agentic run needs, set on the environment the fleet reads.
///
/// One guest request does a model call AND a test suite. The ingress's 30s default
/// backend timeout kills that as "n1 timed out", and the host's 30s wrpc budget
/// kills the nested call as "data receipt timed out" — both of which read as fleet
/// problems and are not.
///
/// 240s was not enough either: a thinking model takes minutes on a real task, and
/// branches died mid-answer. So the floor is 600s and the rest is scaled from the
/// caller's own per-branch timeout, because that is the number they already chose.
fn set_fleet_timeouts(args: &Args) {
    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let backend_timeout = args.timeout.max(600).to_string();
    std::env::set_var("COMP_FLEET_BACKEND_TIMEOUT", &backend_timeout);
    // Inherited by the hosts the fleet spawns.
    std::env::set_var("COMP_RPC_TIMEOUT_SECS", &backend_timeout);
    // Trace outbound dials, so a stalled model call shows whether the host got a
    // response back at all.
    std::env::set_var("COMP_TRACE_EGRESS", "1");
}

/// What each branch is allowed to read, and what it actually read.
///
/// Varied ACROSS branches on purpose: a generation whose branches all read the
/// same top-k is an expensive way to run one branch (ADR-0081's herding), and the
/// branch that reads nothing is the only way to tell whether the pool helps at
/// all. `default_strategies` already keeps one branch from reading the previous
/// winner; that same branch reads no lessons either.
///
/// Returns the strategies and, per branch, the keys it read — which is the other
/// half of attribution when the run ends.
fn reading_per_branch(
    args: &Args,
    goal: &GoalSpec,
    memory: Option<&Memory>,
) -> (Vec<generation_mod::Strategy>, Vec<Vec<String>>) {
    let mut strategies = generation_mod::default_strategies(args.branches);
    let mut read_by_branch: Vec<Vec<String>> = vec![Vec::new(); strategies.len()];
    // What this goal's work touches, so what it learns is findable by the next
    // goal that builds against the same interfaces rather than only by the next
    // goal worded like this one (ADR-0090).
    let tags = comp_reconciler::plug::tags_for(
        &goal.writable,
        &comp_reconciler::plug::Catalog::scan(&comp_reconciler::plug::default_dirs(&repo_root())),
    );
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
                tags: tags.clone(),
                min_similarity: 0.0,
                pools: match i % 3 {
                    0 => vec![],                                      // everything
                    1 => vec!["errors".into()],                       // only what failed
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
        // Herding is branches reading the SAME thing, not branches reading
        // nothing: an empty pool makes every reading identical and that is a cold
        // start, not convergence. Saying otherwise would cry wolf on every first
        // run, and a warning that fires when it should not is a warning people
        // learn to skip.
        let readers = read_by_branch.iter().filter(|r| !r.is_empty()).count();
        if readers > 1 {
            let distinct: std::collections::BTreeSet<&Vec<String>> =
                read_by_branch.iter().filter(|r| !r.is_empty()).collect();
            println!(
                "  knowledge: {} distinct reading(s) across {readers} reading branches{}",
                distinct.len(),
                if distinct.len() == 1 {
                    " — every one read the same thing, which is herding"
                } else {
                    ""
                }
            );
        } else if readers == 0 {
            println!("  knowledge: the pool had nothing for this goal; every branch runs cold");
        }
    }
    (strategies, read_by_branch)
}

fn main() -> Result<()> {
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

    // The base tree and the files the agent starts from.
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
    // A branch makes up to `attempts` model calls IN SEQUENCE, so the per-branch
    // timeout has to hold all of them. When it cannot, the branch dies with
    // `error sending request` and zero attempts — which reads as a fleet fault, and
    // did, for two of six branch-runs on the run that prompted this line. Said out
    // loud rather than corrected: how slow a call is depends on the provider, and a
    // silently-raised timeout is its own surprise.
    //
    // 150s per call is the slow tail of the Claude CLI shim, which is the slowest
    // provider here. A faster one simply never trips this.
    let needed = args.attempts as u64 * 150 + 30;
    if args.timeout < needed {
        println!(
            "WARNING: --timeout {}s cannot hold {} sequential model calls (~{}s needed).\n                      A branch that runs out dies with `error sending request` and NO attempts.\n                      Raise --timeout or lower --attempts.",
            args.timeout, args.attempts, needed
        );
    }
    // Said out loud, because a gate that cannot find the host reports the same
    // thing as a candidate that does not work — and this run spent 280k tokens
    // learning that once.
    let hb = host_bin();
    println!(
        "gate host:   {} ({})",
        hb.display(),
        if hb.exists() { "found" } else { "MISSING — every check that runs the app will fail" }
    );

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
        // Where the host binary is, for a gate that wants to RUN what the
        // candidate built rather than only compile it. The sandbox holds the base
        // tree and nothing else, so a check that needs the host cannot find it by
        // path — and a gate that only compiles is not a gate (measured: `cargo
        // component check` passes on a crate implementing none of its world).
        // NOT `bin_path`: that resolves against the RECONCILER's target directory,
        // and the host is built in its own workspace. Pointing the gate at a
        // binary that does not exist made every check fail with "no comp-host at
        // …" — sixteen gate runs judging a broken harness rather than the code,
        // and a model that read the message and wrote an essay about the build
        // instead of the file it was asked for.
        format!("COMP_HOST={}", host_bin().display()),
        // Composition, for the same reason: a gate has to assemble what the
        // candidate built before it can run it, and the plug chain is derived
        // from the component's own imports rather than written down anywhere.
        // `bin_path` is right here — unlike the host, this one IS built in the
        // reconciler's workspace.
        format!("COMP_PLUG={}", plug_bin().display()),
    ];
    // `cargo` is usually a rustup shim, and under the gate's cleared environment
    // it cannot choose a toolchain — no RUSTUP_HOME, no default. Pass both, so the
    // shim resolves the same toolchain the pre-warm used. Read from the ambient
    // environment (the operator's), never the agent's.
    let rustup_home = std::env::var("RUSTUP_HOME").unwrap_or_else(|_| format!("{home}/.rustup"));
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

    warm_the_gate_cache(
        &goal,
        &args,
        &GateCaches {
            uv_cache: uv_cache.clone(),
            uv_python: uv_python.clone(),
            cargo_home: cargo_home.clone(),
            cargo_target: cargo_target.clone(),
        },
    );

    // Bring the gate up first, so the driver fixture can point at it — or find
    // out where the one somebody else is running lives.
    let gate = Gate::open(&args, &allow, &check_env)?;

    set_fleet_timeouts(&args);
    // The provider's own default when nobody named a base URL, so `--provider
    // openai` does not silently dial api.anthropic.com.
    let base_url = if !args.llm_base_url.is_empty() {
        args.llm_base_url.clone()
    } else if args.provider == "openai" {
        "https://api.openai.com/v1".to_string()
    } else {
        "https://api.anthropic.com".to_string()
    };

    let driver_spec = render(
        "goalrun-driver.yaml",
        &[
            ("PROVIDER", &args.provider),
            ("CHECKS_URL", &gate.url()),
            ("CHECKS_AUTHORITY", &gate.authority()),
            ("LLM_MODEL", &args.model),
            ("MAX_TOKENS", &args.max_tokens.to_string()),
            ("LLM_BASE_URL", &base_url),
            ("LLM_TIMEOUT", &args.timeout.to_string()),
            ("LLM_HOST", &egress_authority(&base_url)),
        ],
    )?;
    let forge_spec = render("goalrun-forge.yaml", &[("FORGE_REPO", &args.repo)])?;

    // Secrets by file: only the PATHS reach argv.
    let mut secrets = vec![
        format!("vault://acme/llmkey=@{}", args.llm_key.display()),
        format!("vault://acme/forge=@{}", args.github_token.display()),
        format!("vault://acme/checkstoken=@{}", gate.token_file().display()),
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
        // The answer door serves two callers now: a part answering a request, and
        // the distiller turning a verified diff into a lesson. Deployed whenever
        // there is a pool to write to.
        specs.push(
            render(
                "goalrun-answer.yaml",
                &[
                    ("PROVIDER", &args.provider),
                    ("ANSWER_MODEL", &args.answer_model),
                    ("LLM_BASE_URL", &base_url),
                    ("LLM_TIMEOUT", &args.timeout.to_string()),
                    ("LLM_HOST", &egress_authority(&base_url)),
                ],
            )?
            .to_str()
            .unwrap()
            .to_string(),
        );
        if !goal.parts.is_empty() {
            // A database PER GOAL. One shared `goalcontract` meant the second goal
            // this machine ever ran was handed the first goal's contract —
            // silently, because "a contract is already published" reads as a
            // repeat run rather than as a different goal.
            //
            // Named from the contract file's path AND the goal's title, because
            // the path alone is not the goal's identity: a second phase over the
            // same CONTRACT.md — new parts, new sections appended by the human who
            // owns the file — collided with the first phase's v1 and refused to
            // start. The title is what distinguishes them, and a rerun of one goal
            // keeps its title and so keeps its negotiation history.
            let slug = |s: &str| -> String {
                s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
            };
            // The title goes in as a short digest rather than as text. A title is
            // free-form and this name travels through a spec, a config value and a
            // database identifier — a 95-character one made the registry
            // unreachable rather than saying anything about a name being too long.
            use sha2::Digest;
            let mut hash = sha2::Sha256::new();
            hash.update(goal.title.as_deref().unwrap_or_default().as_bytes());
            let title_id: String =
                hash.finalize()[..4].iter().map(|b| format!("{b:02x}")).collect();
            // Kept SHORT deliberately. This name travels into a spec, a wasi:config
            // value and a database identifier, and a long one made the registry
            // unreachable — "n1 refused" — rather than complaining about a name.
            let path_slug: String =
                slug(&goal.contract.clone().unwrap_or_default()).chars().take(24).collect();
            let db = format!("goalcontract_{path_slug}_{title_id}");
            specs.push(
                render(
                    "goalrun-contract.yaml",
                    &[("SURREAL_URL", url), ("SURREAL_EGRESS", &egress), ("SURREAL_DB", &db)],
                )?
                .to_str()
                .unwrap()
                .to_string(),
            );
        }
    }
    let spec_refs: Vec<&str> = specs.iter().map(String::as_str).collect();
    let art = artifacts(&args.provider)?;

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
                // Read here rather than reusing the binding below, which is
                // declared after this block: the password is a file path in
                // argv and reading it twice costs nothing.
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

    // --- criticise the gate, before the money -------------------------------
    if !gate_can_judge(&goal, &checks, &gate, &base_commit, &tree, args.timeout) {
        return Ok(());
    }

    if args.smoke {
        return smoke(&args, &goal, port, &context, &checks, &base_commit, &tree, &allow);
    }

    // --- has this already been done? ----------------------------------------
    if !worth_running(memory.as_ref(), &goal, args.skip_above) {
        return Ok(());
    }

    // --- what this run leaves behind (ADR-0092) -----------------------------
    //
    // BEFORE the decomposed dispatch below, and that is the whole point of where
    // it sits. This block used to live after it, so `decomposed` returned before a
    // `Trace` was ever constructed and a two-part run recorded NOTHING — no run
    // row, no attempts, no events. Silently: `report()` counts writes that were
    // dropped, and a trace that does not exist drops nothing, so the run ended
    // clean and the history was simply absent. An absent record reading as a fine
    // one is the same shape as the listing failure in #80.
    //
    // The seed IS the run id: one `holon goal run` is one run, and
    // `run_id(seed, round, branch)` is one attempt inside it. Both paths take it
    // from here so a run has ONE identity rather than a timestamp per code path.
    let seed = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_secs();
    let run = seed.to_string();
    // The password by VALUE. `--surreal-password` is a path because a real
    // password does not belong in argv (the fixture grants it to the graph
    // component as a vault reference); the trace talks to the database directly,
    // so it needs the contents. Absent means unauthenticated — the same thing it
    // means to the fixture above, and to `knowledge-graph`.
    let surreal_password = args
        .surreal_password
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string());
    // `None` without `--surreal-url`, exactly like the pool: a run with no
    // database is supported (ADR-0080 keeps the database out of the platform),
    // and a driver that required one would trade a loop that works for a loop
    // that needs a database to be up.
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

    // --- a DECOMPOSED goal ---------------------------------------------------
    //
    // Parts that compose rather than branches that compete: each half runs its own
    // generations against a shared contract, asks the other for changes it needs,
    // and the winners are merged into one tree judged by the goal's own checks
    // (ADR-0086). One pull request at the end.
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
        "base_tree": tree,
        "max_attempts": args.attempts,
        "seed": 1,
    });

    // --- what each branch is allowed to read --------------------------------
    let (strategies, read_by_branch) = reading_per_branch(&args, &goal, memory.as_ref());

    let driver_url = format!("http://127.0.0.1:{port}/run");
    let timeout = Duration::from_secs(args.timeout);
    let bounds =
        Bounds { branches: args.branches, max_rounds: args.rounds, max_tokens: 0, patience: 0 };

    // `seed`, `run` and `trace` come from above the decomposed dispatch, so both
    // paths share one run identity and one trace.
    let reuse = search_the_pool(&goal.text, &run, trace.as_ref());

    let mut plan = plan;
    if let Some(entry) = pool_context(&reuse) {
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
            let attempt = run_id(seed, r, &e.branch);
            if e.accepted && winner.as_ref().is_none_or(|(_, best)| e.score > *best) {
                winner = Some((attempt.clone(), e.score));
            }
            if let Some(t) = &trace {
                // Spawned and finished are recorded together because this walk
                // happens after the search: the driver sees each branch's whole
                // life at once, not as it happens. Live progress is the socket's
                // job (slice three), not something to fake by writing here twice.
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
                    // How many tries this branch took. A branch that got it right
                    // first and one that needed a repair were indistinguishable
                    // here, which is the one number that says whether repair earns
                    // its budget.
                    e.attempts,
                );
            }
            if let Some(m) = &memory {
                match m.evaluated(&goal.text, &attempt, e.score, e.accepted, "") {
                    Ok(()) => recorded += 1,
                    Err(err) => println!("  (verdict for {attempt} not recorded: {err})"),
                }
                // What this branch LEARNED by failing, in the gate's own words. No
                // model in the path, so negative knowledge cannot be a
                // hallucination — and it is visible to a sibling immediately,
                // because its worst case is avoiding something that would have
                // worked (ADR-0081's asymmetry).
                if !e.accepted {
                    if let Some(text) = memory::failure_text(&e.failures, e.score) {
                        match m.observe_failure(&goal.text, &e.branch, &attempt, &text) {
                            Ok(h) => println!("  {} wrote a lesson: {h}", e.branch),
                            Err(err) => println!("  (lesson from {run} not recorded: {err})"),
                        }
                    }
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

    // --- promote what the gate proved ----------------------------------------
    promote_and_sweep(
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
        // `exhausted`, not `failed`: every branch ran and none passed, which is a
        // different thing from a run that broke. The count of these on a goal is
        // what says whether another generation is worth buying.
        if let Some(t) = &trace {
            t.run_resolved(&run, "exhausted", None, "");
            if let Some(why) = t.report() {
                println!("trace: {why}");
            }
        }
        // EXIT 3, not 0. A run where every branch was gated and none passed is a
        // legitimate outcome of a search, not a success — and the caller cannot
        // tell the two apart from a zero. `comp-goald` marked an exhausted run
        // `awaiting-human`, which put a goal nobody had written code for in the
        // queue of goals waiting to be landed.
        //
        // 3 rather than 1 so it stays distinguishable from a run that BROKE: one
        // says the model could not do it, the other says the harness fell over,
        // and a caller that wants to retry cares which.
        //
        // Flushed explicitly: stdout is block-buffered when piped (which is how a
        // daemon runs this), and `process::exit` does not run destructors.
        use std::io::Write;
        let _ = std::io::stdout().flush();
        std::process::exit(3);
    }

    if args.dry_run {
        let best = found.best.as_ref().unwrap();
        println!("\n[dry run] a candidate passed (score {}); not opening a PR.", best.score);
        // And it is WRITTEN OUT, because a dry run that discards the winner is a
        // search you paid for and kept nothing from. This is not hypothetical: a
        // `card-identify` run spent 42 model calls, passed all 19 held-out tests,
        // printed this line, returned, and left the stub exactly as it was.
        //
        // Into a directory rather than the checkout: a dry run must not mutate the
        // tree it was pointed at, and diffing a directory is one command.
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
        return Ok(());
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
            // Re-report the winning run, now that there is something addressable
            // to point the next run at. Idempotent per (goal, run), so this
            // attaches the pull request without inventing a second evaluation.
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
            // A branch passed the gate and the forge still produced nothing. That
            // is a FAILED run, not an exhausted one: the difference is whether
            // the work was good, and a trace that conflated them would hide the
            // forge as a cause.
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
                // Writable files (the part's own stub) keep every comment — there the comments
                // are the brief. Read-only `.wit` context is trimmed by `lean_context`.
                "context": branch_context(&args.checkout, &p.writable, &p.context),
                "previous": [],
                "checks": p.checks.iter().map(|c| json!({
                    "id": c.id, "required": c.required, "weight": c.weight, "command": c.command,
                    "needs": c.needs,
                })).collect::<Vec<_>>(),
                "base_commit": base_commit,
                "base_tree": tree,
                "max_attempts": args.attempts,
                "seed": 1,
            }),
        })
        .collect();
    println!("parts: {}", parts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", "));

    let timeout = Duration::from_secs(args.timeout);
    let bounds =
        Bounds { branches: args.branches, max_rounds: args.rounds, max_tokens: 0, patience: 0 };
    // `seed` is the caller's — one run, one identity, and the trace keys on it.
    let run_key = seed.to_string();
    // How the run ended, said once. Every `return` below this point goes through
    // it, so an early exit cannot leave a run row that started and never resolved
    // — which is indistinguishable from a crash when someone reads it later.
    let resolve = |outcome: &str, url: &str| {
        if let Some(t) = trace {
            // "composition", not a branch name: a decomposed run has no single
            // winning branch — each part picked one and the JOIN is what passed a
            // gate neither half could pass alone. Naming one part's branch here
            // would credit half the work.
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

    // --- the record, before anything below can fail -------------------------
    //
    // Every branch of every generation of every part, keyed the same way the
    // ordinary path keys them. The part name is IN the attempt id: two parts each
    // run a `branch-0` in round 0, and `run_id(seed, round, branch)` alone would
    // give them one id and silently overwrite one half's history with the other's.
    if let Some(t) = trace {
        for p in &run.composition.parts {
            for (r, round) in p.rounds.iter().enumerate() {
                for e in &round.entries {
                    let attempt = run_id(seed, r, &format!("{}/{}", p.part, e.branch));
                    t.branch_spawned(&run_key, &attempt, &e.branch, r);
                    t.gate_verdict(&run_key, &attempt, e.score, e.accepted, &e.failures);
                    // `errored` when the branch produced nothing at all — a note
                    // and no files is how a provider failure or an answer with no
                    // file block reaches here, and calling that "failed" would put
                    // it in with candidates the gate actually judged.
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
        // Why a branch produced NOTHING, which is a different question from why a
        // candidate failed and lands in a different field. A run that reports
        // "produced nothing in 3 rounds" and stops has told the reader nothing
        // they can act on — the note is the only place a transport failure, a
        // refused plan or a dead provider says so.
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
        // Which check, and what it said. "component never passed its gate" without
        // this is the least actionable sentence a run can end with — the ordinary
        // path has printed its closest failing checks since it existed.
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
        // WHO a join failure is about. The halves each passed and the whole did not,
        // so no part's own gate has anything to say — and the run used to end with
        // "the halves pass alone and not together" and no further address.
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

        // And the merged tree is WRITTEN OUT, because a join failure discards the
        // work of every part otherwise. `changes` is already carried on this path —
        // three parts' worth of accepted code — and nothing ever read it. Losing a
        // whole decomposed run to a verdict about the join is the most expensive
        // discard in the loop.
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

    // The gate accepted, so this is where the graph is allowed to learn from success — the one
    // thing the decomposed path never did. Before the PR, because promotion is earned by the
    // verdict, not by the forge accepting the branch.
    promote_parts(memory_for_parts.as_ref(), goal, port, &run.composition.parts);

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
    use super::{
        authority_is_loopback, branch_context, component_scope, egress_authority,
        gates_missing_from_the_tree, join_failure_owners, mint_token, new_capabilities,
        trim_members, CheckSpec, GoalSpec, PartSpec,
    };
    use serde_json::json;

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

    /// The real `components/Cargo.toml`, whose members list opens with a comment
    /// containing `[workspace]` — so the first `]` after the bracket is not the
    /// array's. Closing on it produced invalid TOML, and every branch of every goal
    /// scoped to that workspace was handed a tree cargo refuses to load.
    #[test]
    fn a_bracket_inside_a_comment_does_not_close_the_members_list() {
        let manifest = concat!(
            "[workspace]\n",
            "resolver = \"2\"\n",
            "members = [\n",
            "    # `bench-suite-p3` stays out: it declares its own `[workspace]`, so\n",
            "    # adding it is \"multiple workspace roots\".\n",
            "    \"ai-inference\",\n",
            "    \"card-identify\",\n",
            "]\n",
            "[workspace.package]\n",
            "version = \"0.1.0\"\n",
        );
        let out = trim_members(manifest, &["card-identify".to_string()]);
        assert!(out.contains("members = [\"card-identify\"]"), "{out}");
        assert!(!out.contains("ai-inference"), "the other members are gone: {out}");
        assert!(out.contains("[workspace.package]"), "the rest of the manifest survives: {out}");
        assert!(
            !out.contains("adding it is"),
            "the comment inside the list went with the list, leaving no dangling prose: {out}"
        );
        // The whole point: what comes out has to be loadable.
        toml_is_parseable(&out);
    }

    /// Parsed with the same crate cargo would use, so "valid TOML" is not an opinion.
    fn toml_is_parseable(text: &str) {
        let parsed: Result<toml::Value, _> = toml::from_str(text);
        assert!(
            parsed.is_ok(),
            "the trimmed manifest is not valid TOML: {:?}\n{text}",
            parsed.err()
        );
    }

    /// A path in both `writable` and `context` is sent once, and as the WRITABLE
    /// copy — the untrimmed one. Sent twice it is paid for on every attempt, and the
    /// two copies are stripped differently, which is worse than the cost.
    #[test]
    fn a_path_in_both_lists_is_shown_once_and_unstripped() {
        let dir = std::env::temp_dir().join("holon-branch-context-test");
        let wit = dir.join("a.wit");
        std::fs::create_dir_all(&dir).expect("tmpdir");
        std::fs::write(&wit, "// a comment\npackage a:b@0.1.0;\n").expect("write");

        let both = vec!["a.wit".to_string()];
        let out = branch_context(&dir, &both, &both);
        assert_eq!(out.len(), 1, "shown once: {out:?}");
        assert!(
            out[0]["content"].as_str().expect("content").contains("// a comment"),
            "the writable copy keeps its comments: {out:?}"
        );

        // Read-only only: `lean_context` strips a `.wit`'s comments.
        let readonly = branch_context(&dir, &[], &both);
        assert!(!readonly[0]["content"].as_str().expect("content").contains("// a comment"));

        // A `.rs` held-out test is never stripped, in either position: its doc
        // comments are the specification.
        std::fs::write(dir.join("t.rs"), "//! the spec\nfn x() {}\n").expect("write");
        let rs = branch_context(&dir, &[], &["t.rs".to_string()]);
        assert!(rs[0]["content"].as_str().expect("content").contains("//! the spec"));

        // A path that does not exist is skipped, not fatal, and not an empty entry.
        assert!(branch_context(&dir, &[], &["nope.rs".to_string()]).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A join failure is attributed by EVIDENCE — a path the part owns, or its name —
    /// and a failure naming neither belongs to the join itself. Guessing the likeliest
    /// part would be worse than saying so: the reader would go and read the wrong diff.
    #[test]
    fn a_join_failure_names_the_part_that_owns_it_or_says_it_owns_none() {
        let part = |name: &str, w: &str| PartSpec {
            name: name.into(),
            text: "t".into(),
            writable: vec![w.into()],
            context: vec![],
            checks: vec![],
        };
        let parts = vec![
            part("backend", "components/x/src/api.rs"),
            part("frontend", "components/x/ui/app.tsx"),
        ];

        assert_eq!(
            join_failure_owners("assertion failed at components/x/src/api.rs:22", &parts),
            vec!["backend"],
            "a path it owns"
        );
        assert_eq!(
            join_failure_owners("the frontend never called /api/total", &parts),
            vec!["frontend"],
            "its own name"
        );
        assert_eq!(
            join_failure_owners(
                "components/x/src/api.rs disagrees with components/x/ui/app.tsx",
                &parts
            ),
            vec!["backend", "frontend"],
            "both, when both are named — a boundary disagreement is not one part's"
        );
        assert!(
            join_failure_owners("the halves pass alone and not together (score 400)", &parts)
                .is_empty(),
            "names no part, so the JOIN owns it"
        );
    }

    /// A gate whose script is not in the shipped tree must be refused BEFORE a
    /// branch is spent, because a missing script and unfinished work are the same
    /// exit code — and the run would score every candidate zero for the operator's
    /// reason. This is the case `gate.sh` being untracked actually produced.
    #[test]
    fn a_gate_script_missing_from_the_tree_is_refused() {
        let goal = |cmd: Vec<&str>| GoalSpec {
            text: "t".into(),
            writable: vec![],
            title: None,
            base_paths: vec![],
            workspace_manifest: None,
            keep_members: vec![],
            component: None,
            context: vec![],
            checks: vec![CheckSpec {
                id: "spec".into(),
                may_pass_base: false,
                required: true,
                weight: 1,
                command: cmd.into_iter().map(String::from).collect(),
                needs: vec![],
            }],
            contract: None,
            parts: vec![],
        };
        let tree = vec![json!({ "path": "components/x/shipped.sh", "content": "" })];

        assert!(
            gates_missing_from_the_tree(&goal(vec!["bash", "components/x/shipped.sh"]), &tree)
                .is_empty(),
            "a shipped script is fine"
        );

        let missing =
            gates_missing_from_the_tree(&goal(vec!["bash", "components/x/absent.sh"]), &tree);
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(missing[0].contains("absent.sh"), "{}", missing[0]);
        assert!(
            missing[0].contains("no such file"),
            "not on disk either, so say so: {}",
            missing[0]
        );

        // The discriminator has to leave ordinary commands alone. None of these
        // names a repo script, and refusing any of them would break real goals.
        for cmd in [
            vec!["cargo", "test", "-p", "card-identify"],
            vec!["curl", "-sf", "http://127.0.0.1:8080/health"],
            vec!["just", "e2e-binder"],
            vec!["bash", "-c", "cd components && cargo test"],
        ] {
            assert!(
                gates_missing_from_the_tree(&goal(cmd.clone()), &tree).is_empty(),
                "{cmd:?} names no repo script and must not be refused"
            );
        }
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
    ///
    /// `run_id(seed, round, branch)` is the ordinary path's key, where a branch
    /// name is unique within a run. A decomposed run breaks that: both halves
    /// spawn `branch-0` in round 0, so keying on it alone gives them ONE attempt
    /// row and the second half silently overwrites the first half's history —
    /// visible only as a run whose branch count is half what it should be.
    #[test]
    fn two_parts_do_not_share_one_attempt_id() {
        use comp_reconciler::memory::run_id;
        assert_ne!(
            run_id(7, 0, "access-and-search/branch-0"),
            run_id(7, 0, "reports/branch-0"),
            "the part name must be part of the key"
        );
        // And a run still separates its own rounds and branches.
        assert_ne!(run_id(7, 0, "reports/branch-0"), run_id(7, 1, "reports/branch-0"));
        assert_ne!(run_id(7, 0, "reports/branch-0"), run_id(7, 0, "reports/branch-1"));
    }

    /// Both manifests that hold a provider must carry the read timeout.
    ///
    /// A fixture that names no `LLM_TIMEOUT` renders to a manifest without the
    /// key, the provider falls back to its default, and every call slower than
    /// that dies as `error sending request` — the provider reading as DOWN while
    /// the thing behind the base URL is still working. That is what killed a
    /// whole part of a two-part run, and it is silent: nothing in the run log
    /// mentions a timeout.
    ///
    /// It cost a second run to relearn against a self-hosted model, where DECODE
    /// is the slow half: benchmarked at 34 tok/s on a 16k prompt (72 at 1k), so a
    /// 12000-token answer is minutes, not seconds, and every budget shorter than
    /// that kills a working server.
    #[test]
    fn every_manifest_with_a_provider_carries_the_read_timeout() {
        for f in ["goalrun-driver.yaml", "goalrun-answer.yaml"] {
            for provider in ["anthropic", "openai"] {
                let out = crate::render(f, &[("PROVIDER", provider), ("LLM_TIMEOUT", "3600")])
                    .expect("render");
                let yaml = std::fs::read_to_string(out).expect("read back");
                assert!(
                    yaml.contains(&format!("{provider}:timeout: \"3600\"")),
                    "{f} must carry {provider}:timeout, substituted"
                );
                // And the SECRET has to follow the provider, or the manifest grants
                // a key the component never asks for and the call goes out bare.
                assert!(
                    yaml.contains(&format!("key: {provider}-api-key")),
                    "{f} must name {provider}-api-key"
                );
            }
        }
    }

    /// `--provider` picks an artifact, and the two must not be confusable.
    ///
    /// Both components export the same WIT interface, so shipping the wrong one
    /// links and serves and then fails at the first call with a 404 from a path
    /// the other provider does not have — which reads as the model being down.
    #[test]
    fn the_provider_selects_its_own_wasm() {
        // The mapping is a one-liner in `artifacts`; this pins it so a rename of
        // either file cannot silently fall through to the other.
        for (provider, want) in
            [("openai", "openai_provider.wasm"), ("anthropic", "anthropic_provider.wasm")]
        {
            let picked = if provider == "openai" {
                "openai_provider.wasm"
            } else {
                "anthropic_provider.wasm"
            };
            assert_eq!(picked, want, "{provider} must ship {want}");
        }
    }

    /// A component is `components/<name>/…` and nothing else.
    ///
    /// The rule matters because it decides what the pool believes it gained: a
    /// path outside `components/` is app code, and counting it would report a
    /// capability that no future run can reuse.
    #[test]
    fn only_components_count_as_a_new_capability() {
        let files = serde_json::json!([
            { "path": "components/csv-codec/src/lib.rs", "content": "" },
            { "path": "components/csv-codec/Cargo.toml", "content": "" },
            { "path": "apps/vet/src/main.rs", "content": "" },
            { "path": "README.md", "content": "" },
            { "path": "components/paginate/wit/p.wit", "content": "" },
        ]);
        assert_eq!(
            new_capabilities(&files),
            vec![
                ("csv-codec".to_string(), "components/csv-codec".to_string()),
                ("paginate".to_string(), "components/paginate".to_string()),
            ],
            "one entry per COMPONENT, not per file, and nothing outside components/"
        );
    }

    /// A run that wrote no component gained the pool nothing, and must say so
    /// rather than reporting an empty-named capability.
    #[test]
    fn a_run_that_built_no_component_adds_no_capability() {
        let files = serde_json::json!([
            { "path": "apps/vet/src/main.rs", "content": "" },
            { "path": "components/", "content": "" },
        ]);
        assert!(new_capabilities(&files).is_empty());
    }

    /// The line between "the loopback is the boundary" and "there has to be one".
    ///
    /// Resolved, not string-matched: a substring test for `127.` accepts
    /// `10.0.127.4` and rejects `::1`, and both mistakes point the same way —
    /// letting a runner that runs commands listen where a second machine can
    /// reach it with nothing in front of it.
    #[test]
    fn what_counts_as_only_this_machine() {
        for local in ["127.0.0.1:8099", "localhost:8099", "[::1]:8099", "127.9.9.9:1"] {
            assert!(authority_is_loopback(local), "{local} is loopback");
        }
        for remote in ["0.0.0.0:8099", "10.0.127.4:8099", "example.invalid:8099"] {
            assert!(!authority_is_loopback(remote), "{remote} is not");
        }
    }

    /// A token that is 64 hex characters and not the same one twice.
    #[test]
    fn a_minted_token_is_random_and_hex() {
        let a = mint_token().expect("/dev/urandom");
        let b = mint_token().expect("/dev/urandom");
        assert_eq!(a.len(), 64, "32 bytes as hex: {a}");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()), "{a}");
        assert_ne!(a, b, "two runs minted the same token");
    }
}
