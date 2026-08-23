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
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    let status = Command::new(&bin)
        .arg("--checkout")
        .arg(&args.checkout)
        .arg("--repo")
        .arg(&args.repo)
        .arg("--goal")
        .arg(&spec)
        .args(&args.goalrun)
        .status()
        .with_context(|| format!("could not run `{bin}` — build it with `just goal-run`"))?;

    if status.success() {
        eprintln!("[goald] {id} DONE -> awaiting-human");
        call(s, "POST", &format!("/api/goals/{id}/review"), None)?;
    } else {
        // 3 is `comp-goalrun`'s "every branch ran, none passed" — the search was
        // healthy and the answer was no. Said differently from a broken harness
        // because the fix is different: one wants a better goal, the other wants
        // someone to look at the machine.
        let reason = match status.code() {
            Some(3) => "no branch passed the gate — the goal needs work, not a retry".to_string(),
            code => format!("comp-goalrun exited {}", code.unwrap_or(-1)),
        };
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

    // Claimed IN THIS PROCESS. The platform's `running` state is not a lease — it
    // is what a person set — so it cannot tell "started, waiting for a runner"
    // apart from "a runner has it". One daemon per project is the assumption, and
    // a second one would double-run every goal.
    //
    // ponytail: in-process claim set. A real lease (a `claimed_by` + expiry on the
    // goal record) is the fix if a second daemon ever becomes a thing.
    let claimed: Arc<Mutex<HashSet<String>>> = Arc::default();
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
                    for goal in goals.into_iter().take(free) {
                        let id = goal["id"].as_str().unwrap_or_default().to_string();
                        if id.is_empty() || !claimed.lock().unwrap().insert(id.clone()) {
                            continue;
                        }
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
