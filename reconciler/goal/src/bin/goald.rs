//! Drains goals a person moved to `running` (never `queued`, ADR-0082) through `comp-goalrun`.
//! `--max-runs` > 1 does not detect two goals writing one file; scope them to disjoint `writable`.

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
    /// The account to sign back in as when the hour-long session expires mid-run.
    #[arg(long)]
    email: Option<String>,
    /// A file holding that account's password, never a value.
    #[arg(long)]
    password_file: Option<PathBuf>,
    /// Hold back DeepSeek-billed goals until its off-peak window (`comp_fuel::offpeak`).
    #[arg(long, default_value_t = false)]
    enforce_deepseek_offpeak: bool,
    /// A file of `YYYY-MM-DD` off-peak days, read only with `--enforce-deepseek-offpeak`.
    #[arg(long)]
    holidays: Option<PathBuf>,
    /// How many goals may be in flight at once; each fans out `--branches` model calls.
    #[arg(long, default_value_t = 1)]
    max_runs: usize,
    /// Seconds between polls of the worklist.
    #[arg(long, default_value_t = 15)]
    poll: u64,
    /// Take one pass over the queue and exit.
    #[arg(long, default_value_t = false)]
    once: bool,
    /// Close an `awaiting-human` goal once its recorded PR is merged (`done`) or closed (`failed`).
    #[arg(long, default_value_t = false)]
    auto_close_merged_prs: bool,
    /// A file holding a GitHub token for `--auto-close-merged-prs`; omit for public repos.
    #[arg(long)]
    github_token_file: Option<PathBuf>,
    /// Everything after `--`, handed to `comp-goalrun` unchanged.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    goalrun: Vec<String>,
}

struct Session {
    url: String,
    /// Replaced in place when the platform stops accepting it, so every caller picks up the new one.
    token: Mutex<String>,
    /// What to sign back in with. `None` means a 401 is simply an error.
    login: Option<(String, String)>,
}

/// The CLI's credentials file, so `comp login` is the only way a token reaches this box.
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
        // A 401 is the one status worth a second attempt, after signing back in.
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

fn started(s: &Session, project: &str) -> Result<Vec<Value>> {
    let v = call(s, "GET", &format!("/api/projects/{project}/goals?state=running"), None)?;
    Ok(v["goals"].as_array().cloned().unwrap_or_default())
}

fn fail_goal(s: &Session, id: &str, reason: &str) -> Result<Value> {
    call(s, "POST", &format!("/api/goals/{id}/fail"), Some(serde_json::json!({ "reason": reason })))
}

#[derive(Debug, PartialEq, Eq)]
enum PrStatus {
    Open,
    Merged,
    ClosedUnmerged,
}

/// The shape `github-forge` returns, the only shape ever stored in `pr`.
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

/// A fact about the forge, not anything `comp-goald` keeps.
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

fn sweep_awaiting_human(
    http: &reqwest::blocking::Client,
    token: Option<&str>,
    s: &Session,
    project: &str,
) {
    let goals = match call(
        s,
        "GET",
        &format!("/api/projects/{project}/goals?state=awaiting-human"),
        None,
    ) {
        Ok(v) => v["goals"].as_array().cloned().unwrap_or_default(),
        Err(e) => {
            eprintln!("[goald] sweep: polling awaiting-human: {e:#}");
            return;
        }
    };
    for goal in goals {
        let id = goal["id"].as_str().unwrap_or_default().to_string();
        let Some(pr) = goal["pr"].as_str() else {
            // Nothing recorded: it waits for a person.
            continue;
        };
        match pr_status(http, token, pr) {
            Ok(PrStatus::Merged) => {
                eprintln!("[goald] {id} merged ({pr}) -> done");
                let _ = call(s, "POST", &format!("/api/goals/{id}/done"), None);
            }
            Ok(PrStatus::ClosedUnmerged) => {
                eprintln!("[goald] {id} closed without merging ({pr}) -> failed");
                let _ = fail_goal(s, &id, &format!("pull request closed without merging: {pr}"));
            }
            Ok(PrStatus::Open) => {}
            Err(e) => eprintln!("[goald] {id} checking {pr}: {e:#}"),
        }
    }
}

/// Success is `awaiting-human` (`done` is the human's); failure is terminal `failed` (ADR-0082).
fn work(args: &Args, s: &Session, goal: &Value) -> Result<()> {
    let id = goal["id"].as_str().unwrap_or_default().to_string();
    let title = goal["title"].as_str().unwrap_or("(untitled)").to_string();
    // The frozen spec: what a run is judged against must not move under it (ADR-0081).
    let spec = goal["frozen_spec"]
        .as_str()
        .filter(|v| !v.is_empty())
        .or_else(|| goal["spec"].as_str())
        .unwrap_or_default()
        .to_string();
    if spec.is_empty() {
        let why = "the goal names no spec file — a run needs a goal.toml in the repo";
        eprintln!("[goald] {id} SKIPPED: {why}");
        let _ = fail_goal(s, &id, why);
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
        // Piped, to catch the "PR opened: <url>" line; echoed below so the daemon log is unchanged.
        .stdout(std::process::Stdio::piped())
        // Its own process group: a supervisor's stop to the daemon must not reach in-flight work,
        // and `kill(-pid)` can sweep it after `wait()`.
        .process_group(0)
        .spawn()
        .with_context(|| {
            format!(
                "could not run `{bin}` — build it with `cargo build --release --bin \
                 comp-goalrun` in reconciler/, then put it on PATH or set COMP_GOALRUN_BIN"
            )
        })?;
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

    // Only orphans remain (goalrun killed by a signal skips Drop); shelling out avoids `unsafe`.
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{child_pgid}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    if status.success() {
        eprintln!(
            "[goald] {id} DONE -> awaiting-human{}",
            pr.as_deref().map(|u| format!(" ({u})")).unwrap_or_default()
        );
        call(s, "POST", &format!("/api/goals/{id}/review"), Some(serde_json::json!({ "pr": pr })))?;
    } else {
        // Exhausted and broken read differently because the fix differs.
        let reason = comp_goal::goalexit::failure_reason(status.code());
        eprintln!("[goald] {id} FAILED: {reason}");
        fail_goal(s, &id, &reason)?;
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = Arc::new(Args::parse());
    if args.max_runs == 0 {
        bail!("--max-runs 0 would poll forever and run nothing");
    }
    // Read once, so a password never reaches argv or a log line.
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

    // Once at startup: the trailing goalrun args, and so the model, never change between polls.
    let off_peak_days = match &args.holidays {
        Some(path) => comp_fuel::offpeak::parse_holidays(
            &std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?,
        )
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?,
        None => Vec::new(),
    };
    let model = comp_fuel::offpeak::model_arg(&args.goalrun).map(str::to_string);
    if args.enforce_deepseek_offpeak {
        match &model {
            Some(m) if comp_fuel::offpeak::is_deepseek_model(m) => {
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

    // In-process: `running` is not a lease, so a second daemon would double-run every goal.
    // ponytail: in-process claim set; a real lease (claimed_by + expiry) if a second daemon runs.
    let claimed: Arc<Mutex<HashSet<String>>> = Arc::default();
    // So the DEFERRED/starting lines log once per transition, not per poll.
    let deferred: Arc<Mutex<HashSet<String>>> = Arc::default();
    let running = Arc::new(AtomicUsize::new(0));

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        // First Ctrl-C stops picking up work; a killed run would leave its goal stuck in `running`.
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
                    // Not `.take(free)`: a deferred goal would waste a free slot.
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
                        if comp_fuel::offpeak::should_defer(
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
                        let (args, s, running, claimed) =
                            (args.clone(), s.clone(), running.clone(), claimed.clone());
                        std::thread::spawn(move || {
                            match work(&args, &s, &goal) {
                                // The goal has left `running`: if it is ever queued again, run it.
                                Ok(()) => {
                                    claimed.lock().unwrap().remove(&id);
                                }
                                // Logged, not fatal: one goal must not take the daemon down. It stays
                                // claimed: its state did not move, and running it again every poll
                                // would spend on a goal the platform cannot be told about.
                                Err(e) => eprintln!("[goald] {id} ERROR: {e:#}"),
                            }
                            running.fetch_sub(1, Ordering::SeqCst);
                        });
                    }
                }
                Err(e) => eprintln!("[goald] polling: {e:#}"),
            }
        }

        if args.auto_close_merged_prs {
            sweep_awaiting_human(&http, github_token.as_deref(), &s, &args.project);
        }

        if args.once || (stop.load(Ordering::SeqCst) && running.load(Ordering::SeqCst) == 0) {
            break;
        }
        std::thread::sleep(Duration::from_secs(args.poll));
    }

    // `--once` returns before its runs do; exiting would take every child with it.
    while running.load(Ordering::SeqCst) > 0 {
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

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

    /// Live and read-only against a real merged PR: mocking GitHub's JSON only tests the mock.
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
