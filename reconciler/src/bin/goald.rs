//! `comp-goald` — the thing that drains the queue.
//!
//! ADR-0082 built a worklist and deliberately left it to a person to pull from:
//! no runner loop, one active run per project. This is the loop that ADR left
//! out, kept as narrow as the argument for leaving it out allows.
//!
//! **A human still starts every goal.** The daemon picks up goals already in
//! `running` — the state a person (or the console) moved them to — and never
//! touches `queued`. That keeps ADR-0082's one deliberate act per goal, which is
//! the whole reason the interruption rate is measurable, and removes only the
//! part nobody wanted: sitting at a terminal typing `goal run` one at a time.
//!
//! What it does change is **one active run per project**, which ADR-0082 answered
//! concurrent pull requests with. `--max-runs` above 1 means concurrent PRs are
//! back, off one base. They are independent branches off the same sha, so the
//! forge is fine; what is not fine is two goals writing the same file, and
//! nothing here detects that. Scope goals to disjoint `writable` sets.
//!
//! ## What the goals know about each other
//!
//! Nothing, directly — and that is the design. Every run shares one knowledge
//! pool (`--surreal-url`, passed through to `comp-goalrun`), so a goal reads the
//! lessons, capabilities and verdicts every earlier goal left there, and a goal
//! whose work a past run already did is skipped before it spends a branch
//! (`--skip-above`). Concurrent runs see each other's writes only once they land,
//! which is the honest behaviour: a lesson from a branch that has not been gated
//! yet is not a lesson.
//!
//!   comp-goald --project holon --checkout ~/src/holon --repo me/holon \
//!     --max-runs 2 -- --anthropic-base-url http://127.0.0.1:8787 --model qwen …
//!
//! Everything after `--` is handed to `comp-goalrun` verbatim. The daemon adds
//! `--checkout`, `--repo` and `--goal`; every other decision — the model, the
//! budget, the pool, the branch count — stays where it already is, and this
//! binary does not grow a second copy of it that can disagree.

use std::collections::HashSet;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use clap::Parser;
use serde_json::Value;

#[derive(Parser)]
#[command(name = "comp-goald", about = "Drain a project's goal queue, N runs at a time.")]
struct Args {
    /// The project whose queue this drains.
    #[arg(long)]
    project: String,
    /// A local checkout of that project's repository.
    #[arg(long)]
    checkout: PathBuf,
    /// `owner/name` of the repository the PRs open on.
    #[arg(long)]
    repo: String,
    /// The platform. Defaults to whatever `comp login` stored.
    #[arg(long)]
    platform_url: Option<String>,
    /// The account to sign back in as when the session expires.
    ///
    /// A platform session lasts an hour. A daemon is meant to outlive that by
    /// days, and without these it does not: measured, a run that started inside a
    /// valid session finished outside one, and the 401 landed on the call that
    /// REPORTS the result — so the work was done, the outcome was lost, and the
    /// goal sat in `running` with nothing coming for it.
    #[arg(long)]
    email: Option<String>,
    /// A FILE holding that account's password. Never a value — a path.
    #[arg(long)]
    password_file: Option<PathBuf>,
    /// Hold back a goal whose `--model` is billed under DeepSeek's
    /// clock-dependent pricing (see `comp_reconciler::offpeak`) until DeepSeek's
    /// off-peak window opens, instead of spending it at up to 2x the price.
    ///
    /// Off by default: `comp-offpeak` already exists as a one-shot cron gate for
    /// `comp-goalrun`, but `comp-goald` is a continuous poll loop that never goes
    /// through cron, so nothing enforced the schedule against it — a person
    /// starting a goal at 14:00 UTC got charged the peak rate with no warning.
    /// Turning this on makes a DeepSeek-backed daemon actually wait, the same
    /// discount `cost.rs` already knows how to price but never gated on.
    #[arg(long, default_value_t = false)]
    enforce_deepseek_offpeak: bool,
    /// A file of `YYYY-MM-DD` lines (Chinese public holidays / adjusted working
    /// days) that are off-peak all day — see `comp-offpeak --help`. Only read
    /// when `--enforce-deepseek-offpeak` is set.
    #[arg(long)]
    holidays: Option<PathBuf>,
    /// How many goals may be in flight at once.
    ///
    /// Every run is itself a fan-out of `--branches`, so this multiplies: 2 runs
    /// of 4 branches is 8 concurrent model calls. Whether that is parallelism or
    /// a queue depends on the server — measured against mlx_lm, four sequences
    /// were in flight at once, so it batches and this buys real concurrency. A
    /// server that answers serially turns the same number into every run getting
    /// slower by the same factor, with timeouts that are harder to read.
    #[arg(long, default_value_t = 1)]
    max_runs: usize,
    /// Seconds between polls of the worklist.
    #[arg(long, default_value_t = 15)]
    poll: u64,
    /// Take one pass over the queue and exit. For a cron, and for testing that
    /// the wiring works without leaving something running.
    #[arg(long, default_value_t = false)]
    once: bool,
    /// Close an `awaiting-human` goal automatically once its recorded pull
    /// request is merged (`done`) or closed unmerged (`failed`), instead of
    /// waiting for a person to do it by hand.
    ///
    /// Off by default: this changes what the daemon does WITHOUT being asked
    /// each time, same reasoning as `--enforce-deepseek-offpeak`. The human
    /// decision this reflects already happened on GitHub when the PR was
    /// reviewed and merged — this only stops the platform's own record from
    /// lagging behind that fact. A goal recorded with no `pr` (an older
    /// review, or one from a client that never sent it) is left alone; there
    /// is nothing here to check.
    #[arg(long, default_value_t = false)]
    auto_close_merged_prs: bool,
    /// A FILE holding a GitHub token, for the pull-request status checks
    /// `--auto-close-merged-prs` makes. Omit for public repos — GitHub's API
    /// answers those unauthenticated, just at a much lower rate limit.
    #[arg(long)]
    github_token_file: Option<PathBuf>,
    /// Everything after `--`, handed to `comp-goalrun` unchanged.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    goalrun: Vec<String>,
}

// ---- the platform, as the four calls this needs -----------------------------

struct Session {
    url: String,
    /// Replaced in place when the platform stops accepting it, so every caller
    /// picks the new one up without threading it through.
    token: Mutex<String>,
    /// What to sign back in with. `None` means a 401 is simply an error, which is
    /// the right behaviour for a daemon nobody gave credentials to.
    login: Option<(String, String)>,
}

/// The same credentials file the CLI writes, read the same way — so `comp login`
/// is the only way a token gets onto this box, and the daemon does not become a
/// second place that knows how to authenticate.
fn session(override_url: Option<String>, login: Option<(String, String)>) -> Result<Session> {
    let p = std::env::var("COMP_CREDENTIALS").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
            .join(".config/comp/credentials.json")
    });
    let raw = std::fs::read(&p)
        .with_context(|| format!("no session at {} — run `holon login` first", p.display()))?;
    let v: Value = serde_json::from_slice(&raw).context("credentials file is not readable JSON")?;
    Ok(Session {
        url: override_url
            .or_else(|| v["url"].as_str().map(String::from))
            .unwrap_or_else(|| "http://127.0.0.1:8080".into()),
        token: Mutex::new(v["token"].as_str().unwrap_or_default().to_string()),
        login,
    })
}

/// Trade the credentials for a fresh token and store it.
fn re_login(s: &Session) -> Result<()> {
    let Some((email, password)) = &s.login else {
        bail!("the session expired and no --email/--password-file was given to renew it");
    };
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build()?;
    let res = http
        .post(format!("{}/api/login", s.url.trim_end_matches('/')))
        .json(&serde_json::json!({ "email": email, "password": password }))
        .send()?;
    if !res.status().is_success() {
        bail!("signing back in: {} {}", res.status(), res.text().unwrap_or_default().trim());
    }
    let v: Value = res.json()?;
    let token = v["token"].as_str().unwrap_or_default().to_string();
    if token.is_empty() {
        bail!("the platform returned no token");
    }
    *s.token.lock().unwrap() = token;
    eprintln!("[goald] session renewed");
    Ok(())
}

fn call(s: &Session, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
    match call_once(s, method, path, body.clone()) {
        // A 401 is the one status worth a second attempt, and only after doing
        // something about it. Retrying anything else would just repeat it.
        Err(e) if e.to_string().starts_with("401") && s.login.is_some() => {
            re_login(s)?;
            call_once(s, method, path, body)
        }
        other => other,
    }
}

fn call_once(s: &Session, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build()?;
    let url = format!("{}{}", s.url.trim_end_matches('/'), path);
    let token = s.token.lock().unwrap().clone();
    let mut req = match method {
        "GET" => http.get(&url),
        _ => http.post(&url),
    }
    .bearer_auth(token);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let res = req.send().with_context(|| format!("calling {url}"))?;
    let status = res.status();
    let text = res.text().unwrap_or_default();
    if !status.is_success() {
        bail!("{status} from {path}: {}", text.trim());
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// The goals a person has started and nothing has picked up yet.
fn started(s: &Session, project: &str) -> Result<Vec<Value>> {
    let v = call(s, "GET", &format!("/api/projects/{project}/goals?state=running"), None)?;
    Ok(v["goals"].as_array().cloned().unwrap_or_default())
}

// ---- closing the loop on a merged (or abandoned) pull request ---------------

/// What GitHub says about a pull request right now.
#[derive(Debug, PartialEq, Eq)]
enum PrStatus {
    Open,
    Merged,
    ClosedUnmerged,
}

/// `owner`, `repo`, `number` out of a URL `comp-goalrun` printed — the same
/// shape `github-forge` returns and the only shape ever stored in `pr`.
fn parse_pr_url(url: &str) -> Option<(String, String, String)> {
    let rest = url.trim().trim_end_matches('/').strip_prefix("https://github.com/")?;
    let mut parts = rest.splitn(4, '/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    if parts.next()? != "pull" {
        return None;
    }
    let number = parts.next()?.to_string();
    Some((owner, repo, number))
}

/// Ask GitHub directly — this is a fact about the FORGE, not about anything
/// `comp-goald` itself keeps, so there is no reason to route it through the
/// platform or through a wasm component that cannot make outbound calls of
/// its own choosing anyway.
fn pr_status(http: &reqwest::blocking::Client, token: Option<&str>, url: &str) -> Result<PrStatus> {
    let (owner, repo, number) =
        parse_pr_url(url).with_context(|| format!("`{url}` does not look like a GitHub PR URL"))?;
    let mut req = http
        .get(format!("https://api.github.com/repos/{owner}/{repo}/pulls/{number}"))
        .header("User-Agent", "comp-goald")
        .header("Accept", "application/vnd.github+json");
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let res = req.send().context("calling the GitHub API")?;
    let status = res.status();
    let v: Value = res.json().context("the GitHub API did not answer with JSON")?;
    if !status.is_success() {
        bail!("{status} from GitHub: {}", v["message"].as_str().unwrap_or_default());
    }
    Ok(if v["merged"].as_bool().unwrap_or(false) {
        PrStatus::Merged
    } else if v["state"].as_str() == Some("closed") {
        PrStatus::ClosedUnmerged
    } else {
        PrStatus::Open
    })
}

/// One pass over every `awaiting-human` goal with a recorded PR: land it if
/// GitHub says it merged, dead-letter it if GitHub says it closed without
/// merging, leave it alone otherwise. The human decision already happened on
/// GitHub — this only keeps the platform's own record from lagging behind it.
fn sweep_awaiting_human(
    http: &reqwest::blocking::Client,
    token: Option<&str>,
    s: &Session,
    project: &str,
) {
    let goals = match call(s, "GET", &format!("/api/projects/{project}/goals?state=awaiting-human"), None) {
        Ok(v) => v["goals"].as_array().cloned().unwrap_or_default(),
        Err(e) => {
            eprintln!("[goald] sweep: polling awaiting-human: {e:#}");
            return;
        }
    };
    for goal in goals {
        let id = goal["id"].as_str().unwrap_or_default().to_string();
        let Some(pr) = goal["pr"].as_str() else {
            // Nothing recorded — an older review, or one from a client that
            // never sent it. There is nothing here to check, so it waits for
            // a person exactly as it always has.
            continue;
        };
        match pr_status(http, token, pr) {
            Ok(PrStatus::Merged) => {
                eprintln!("[goald] {id} merged ({pr}) -> done");
                let _ = call(s, "POST", &format!("/api/goals/{id}/done"), None);
            }
            Ok(PrStatus::ClosedUnmerged) => {
                eprintln!("[goald] {id} closed without merging ({pr}) -> failed");
                let _ = call(
                    s,
                    "POST",
                    &format!("/api/goals/{id}/fail"),
                    Some(serde_json::json!({ "reason": format!("pull request closed without merging: {pr}") })),
                );
            }
            Ok(PrStatus::Open) => {}
            Err(e) => eprintln!("[goald] {id} checking {pr}: {e:#}"),
        }
    }
}

// ---- one goal ---------------------------------------------------------------

/// Run one goal to a pull request and report where it landed.
///
/// `running -> awaiting-human` on success, because a PR is not a finished goal:
/// somebody still reads it and merges it, and `done` is theirs to set. On failure
/// `running -> failed`, which is TERMINAL by ADR-0082 — this does not retry, and
/// the reason travels with the goal so the dead-letter queue is readable.
fn work(args: &Args, s: &Session, goal: &Value) -> Result<()> {
    let id = goal["id"].as_str().unwrap_or_default().to_string();
    let title = goal["title"].as_str().unwrap_or("(untitled)").to_string();
    // The FROZEN spec, not the live one: ADR-0081 says the spec a run is judged
    // against must not move under it, and `goal_transition` froze it at start.
    let spec = goal["frozen_spec"]
        .as_str()
        .filter(|v| !v.is_empty())
        .or_else(|| goal["spec"].as_str())
        .unwrap_or_default()
        .to_string();
    if spec.is_empty() {
        let why = "the goal names no spec file — a run needs a goal.toml in the repo";
        eprintln!("[goald] {id} SKIPPED: {why}");
        let _ = call(
            s,
            "POST",
            &format!("/api/goals/{id}/fail"),
            Some(serde_json::json!({ "reason": why })),
        );
        return Ok(());
    }

    let bin = std::env::var("COMP_GOALRUN_BIN").unwrap_or_else(|_| "comp-goalrun".into());
    eprintln!("[goald] {id} START {title} ({spec})");
    let mut child = Command::new(&bin)
        .arg("--checkout")
        .arg(&args.checkout)
        .arg("--repo")
        .arg(&args.repo)
        .arg("--goal")
        .arg(&spec)
        .args(&args.goalrun)
        // Piped, not inherited: this is the only way to see the "PR opened:
        // <url>" line `comp-goalrun` prints on a win, so the platform can be
        // told which pull request a goal is actually waiting on instead of
        // losing that fact the moment this process's stdout scrolls past.
        // Echoed straight through below, so a human watching the daemon's own
        // log still sees everything `comp-goalrun` printed, unchanged.
        .stdout(std::process::Stdio::piped())
        // Its own process group, not this daemon's: `comp-goalrun` spawns a
        // whole internal fleet (comp-checks, lattice comp-host nodes,
        // nats-server), and without this every one of them shares
        // `comp-goald`'s own group by default. That would mean a signal aimed
        // at the DAEMON's group — the exact shape a supervisor's stop sends —
        // reaches in-flight work too, straight through the graceful-drain
        // logic below this function that exists specifically to prevent that.
        // A distinct group is also what makes the sweep after `wait()` below
        // possible: `kill(-pid, …)` targets a process group, not a single pid.
        .process_group(0)
        .spawn()
        .with_context(|| format!("could not run `{bin}` — build it with `just goal-run`"))?;
    let child_pgid = child.id() as i32;
    let stdout = child.stdout.take().expect("stdout was piped");
    let pr: Arc<Mutex<Option<String>>> = Arc::default();
    let echo = {
        let pr = pr.clone();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                println!("{line}");
                if let Some(url) = line.trim().strip_prefix("PR opened: ") {
                    *pr.lock().unwrap() = Some(url.to_string());
                }
            }
        })
    };
    let status = child.wait().with_context(|| format!("waiting on `{bin}`"))?;
    let _ = echo.join();
    let pr = pr.lock().unwrap().clone();

    // The guaranteed sweep. `comp-goalrun` itself is confirmed dead by
    // `wait()` above, so this only ever reaches ORPHANED descendants — the
    // fleet's own `Drop`-based cleanup (`gate.rs`'s `Checks`, `fleet.rs`'s
    // `Fleet`) already covers every normal exit path; this is what still
    // catches it if that process was itself killed by a signal (which skips
    // `Drop` exactly the way `std::process::exit` used to) or crashes in some
    // way nobody has thought of yet. "no such process" — nothing left to
    // kill — is the expected, common outcome, so the result is discarded.
    //
    // Shelled out to the `kill` utility rather than calling the syscall
    // directly: the negative-pid form (a process GROUP, not one pid) is the
    // same either way, and this way nothing here needs `unsafe`. `--` stops
    // `-<pgid>` from being parsed as another flag. Output silenced: "no such
    // process" is the expected result on every normal run — the fleet's own
    // Drop-based cleanup already got there first — and printing that every
    // single time would drown the log in routine noise.
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{child_pgid}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    if status.success() {
        eprintln!("[goald] {id} DONE -> awaiting-human{}", pr.as_deref().map(|u| format!(" ({u})")).unwrap_or_default());
        call(s, "POST", &format!("/api/goals/{id}/review"), Some(serde_json::json!({ "pr": pr })))?;
    } else {
        // 3 is `comp-goalrun`'s "every branch ran, none passed" — the search was
        // healthy and the answer was no. Said differently from a broken harness
        // because the fix is different: one wants a better goal, the other wants
        // someone to look at the machine.
        let reason = comp_reconciler::goalexit::failure_reason(status.code());
        eprintln!("[goald] {id} FAILED: {reason}");
        call(
            s,
            "POST",
            &format!("/api/goals/{id}/fail"),
            Some(serde_json::json!({ "reason": reason })),
        )?;
    }
    Ok(())
}

// ---- the loop ---------------------------------------------------------------

fn main() -> Result<()> {
    let args = Arc::new(Args::parse());
    if args.max_runs == 0 {
        bail!("--max-runs 0 would poll forever and run nothing");
    }
    // Read here, once, so a password never reaches argv or a log line.
    let login = match (&args.email, &args.password_file) {
        (Some(e), Some(f)) => Some((
            e.clone(),
            std::fs::read_to_string(f)
                .with_context(|| format!("reading {}", f.display()))?
                .trim()
                .to_string(),
        )),
        _ => None,
    };
    if login.is_none() {
        eprintln!(
            "[goald] no --email/--password-file: this daemon dies when the session expires (~1h)"
        );
    }
    let s = Arc::new(session(args.platform_url.clone(), login)?);
    eprintln!(
        "[goald] {} <- {} every {}s, {} at a time",
        args.project, s.url, args.poll, args.max_runs
    );

    // Read once at startup: every run this daemon starts shares the same
    // trailing `comp-goalrun` args, so the model — and whether it is
    // DeepSeek's — never changes between polls.
    let off_peak_days = match &args.holidays {
        Some(path) => comp_reconciler::offpeak::parse_holidays(
            &std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?,
        )
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?,
        None => Vec::new(),
    };
    let model = comp_reconciler::offpeak::model_arg(&args.goalrun).map(str::to_string);
    if args.enforce_deepseek_offpeak {
        match &model {
            Some(m) if comp_reconciler::offpeak::is_deepseek_model(m) => {
                eprintln!("[goald] enforcing DeepSeek off-peak scheduling for model {m}");
            }
            _ => eprintln!(
                "[goald] --enforce-deepseek-offpeak set but no DeepSeek --model was found in the trailing goalrun args — has no effect"
            ),
        }
    }
    let github_token = args
        .github_token_file
        .as_ref()
        .map(|f| std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display())))
        .transpose()?
        .map(|t| t.trim().to_string());
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build()?;
    if args.auto_close_merged_prs {
        eprintln!("[goald] auto-closing awaiting-human goals once their pull request resolves");
    }

    // Claimed IN THIS PROCESS. The platform's `running` state is not a lease — it
    // is what a person set — so it cannot tell "started, waiting for a runner"
    // apart from "a runner has it". One daemon per project is the assumption, and
    // a second one would double-run every goal.
    //
    // ponytail: in-process claim set. A real lease (a `claimed_by` + expiry on the
    // goal record) is the fix if a second daemon ever becomes a thing.
    let claimed: Arc<Mutex<HashSet<String>>> = Arc::default();
    // Goals currently held back by `--enforce-deepseek-offpeak`. Tracked only so
    // the DEFERRED/starting log lines fire once per transition instead of once
    // per poll — with the default 15s poll a goal held for hours would otherwise
    // print itself out of the log entirely.
    let deferred: Arc<Mutex<HashSet<String>>> = Arc::default();
    let running = Arc::new(AtomicUsize::new(0));

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        // First Ctrl-C stops PICKING UP work; runs already in flight finish. A
        // killed run leaves a goal stuck in `running` with a half-open PR, which
        // is the one state nobody can tell from a run still going.
        ctrlc_ish(move || {
            eprintln!("[goald] draining — in-flight runs finish, nothing new starts (Ctrl-C again to kill)");
            stop.store(true, Ordering::SeqCst);
        });
    }

    loop {
        let free = args.max_runs.saturating_sub(running.load(Ordering::SeqCst));
        if free > 0 && !stop.load(Ordering::SeqCst) {
            match started(&s, &args.project) {
                Ok(goals) => {
                    // NOT `.take(free)`: a deferred goal spawns nothing, so
                    // truncating the candidate list before checking the
                    // schedule would waste a free slot on a goal this pass
                    // was never going to start, and starve one that could.
                    let mut started_this_pass = 0;
                    for goal in goals {
                        if started_this_pass >= free {
                            break;
                        }
                        let id = goal["id"].as_str().unwrap_or_default().to_string();
                        if id.is_empty() || claimed.lock().unwrap().contains(&id) {
                            continue;
                        }
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        if comp_reconciler::offpeak::should_defer(
                            now,
                            &off_peak_days,
                            args.enforce_deepseek_offpeak,
                            model.as_deref(),
                        ) {
                            if deferred.lock().unwrap().insert(id.clone()) {
                                eprintln!(
                                    "[goald] {id} DEFERRED: {} is peak-priced right now, waiting for off-peak",
                                    model.as_deref().unwrap_or("?")
                                );
                            }
                            continue;
                        }
                        if deferred.lock().unwrap().remove(&id) {
                            eprintln!("[goald] {id} off-peak now, starting");
                        }
                        if !claimed.lock().unwrap().insert(id.clone()) {
                            continue;
                        }
                        started_this_pass += 1;
                        running.fetch_add(1, Ordering::SeqCst);
                        let (args, s, running) = (args.clone(), s.clone(), running.clone());
                        std::thread::spawn(move || {
                            if let Err(e) = work(&args, &s, &goal) {
                                // Logged, not fatal: one goal that could not be
                                // reported on must not take the daemon down and
                                // strand every other run in flight.
                                eprintln!("[goald] {id} ERROR: {e:#}");
                            }
                            running.fetch_sub(1, Ordering::SeqCst);
                        });
                    }
                }
                Err(e) => eprintln!("[goald] polling: {e:#}"),
            }
        }

        // Independent of the dispatch above and of `--max-runs`: closing a
        // finished goal frees nothing this daemon is itself holding, so it
        // is not gated on a free slot — only on whether anyone asked for it.
        if args.auto_close_merged_prs {
            sweep_awaiting_human(&http, github_token.as_deref(), &s, &args.project);
        }

        if args.once || (stop.load(Ordering::SeqCst) && running.load(Ordering::SeqCst) == 0) {
            break;
        }
        std::thread::sleep(Duration::from_secs(args.poll));
    }

    // `--once` returns before its runs do; without this the process exits and
    // takes every child with it, which looks exactly like a run that crashed.
    while running.load(Ordering::SeqCst) > 0 {
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

/// Ctrl-C, without pulling in a crate for one signal.
fn ctrlc_ish(f: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let _ = tokio::signal::ctrl_c().await;
            f();
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pr_url_reads_owner_repo_and_number() {
        assert_eq!(
            parse_pr_url("https://github.com/acme/widgets/pull/42"),
            Some(("acme".into(), "widgets".into(), "42".into()))
        );
        // A trailing slash, or one `comp-goalrun`'s own println leaves in from
        // formatting — trimmed rather than refused.
        assert_eq!(
            parse_pr_url("https://github.com/acme/widgets/pull/42/"),
            Some(("acme".into(), "widgets".into(), "42".into()))
        );
    }

    #[test]
    fn parse_pr_url_refuses_anything_that_is_not_a_pull_request_link() {
        for bad in [
            "https://gitlab.com/acme/widgets/pull/42",
            "https://github.com/acme/widgets/issues/42",
            "https://github.com/acme/widgets",
            "not a url at all",
            "",
        ] {
            assert_eq!(parse_pr_url(bad), None, "{bad:?} should not parse");
        }
    }

    /// Live, read-only, against a PR from this very session (`markkovari/holon`
    /// #273 — the marketplace-domain build, merged 2026-09-19) — a fixture
    /// nothing else in this file could construct, since "GitHub's own JSON
    /// shape" is the thing this function translates and mocking it would only
    /// test that the mock agrees with itself. Skipped without network instead
    /// of failing, the same discipline `deepseek_live.rs` uses for its own
    /// live provider check.
    #[test]
    fn pr_status_reads_a_real_merged_pull_request() {
        let http = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap();
        match pr_status(&http, None, "https://github.com/markkovari/holon/pull/273") {
            Ok(status) => assert_eq!(status, PrStatus::Merged, "PR #273 was merged this session"),
            Err(e) => eprintln!("skipping: no network or GitHub unreachable: {e:#}"),
        }
    }
}
