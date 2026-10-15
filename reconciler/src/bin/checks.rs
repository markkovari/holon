//! `comp-checks` — run a candidate's checks, and report what passed.
//!
//! ## Why this is a process and not a component
//!
//! A gate has to run the project's own tests, and a component cannot spawn a process (the sandbox
//! working). So it is native and reached over HTTP like the database and model provider (ADR-0008).
//! It is deliberately dumb: it materialises files over a base directory, runs commands, and reports.
//!
//! ## The check vector, not a verdict
//!
//! It returns EVERY check's result, not pass/fail, because the caller needs both halves (ADR-0081):
//! `required` all passed is the gate; the weighted fraction of ALL checks is the score. A binary gate
//! gives no gradient in the generation where nothing passes yet, so 3-of-11 beats 1-of-11.
//!
//! ## What it refuses
//!
//! Paths that leave the tree, and commands it was not configured to allow: an agent that can name
//! `../../etc` or run an arbitrary shell string on the machine holding the fleet's credentials is
//! remote code execution with extra steps.
//!
//! ## Off the loopback, a token is not optional
//!
//! `--allow` bounds the COMMAND, not the TREE: `cargo test` runs the tree's `build.rs`. Loopback holds
//! that closed, so binding anywhere else without `--token-file` is refused at startup.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};

#[derive(Parser, Debug)]
#[command(name = "comp-checks", about = "Run a candidate's checks in a throwaway tree")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8face")]
    addr: String,

    /// A checkout to lay candidates over, when there is one.
    /// OPTIONAL, and mostly not what you want: a directory pins this runner to a machine that already has
    /// the repository. Prefer sending the tree, posted once per base commit (see `base_commit` on the
    /// request).
    #[arg(long)]
    base: Option<PathBuf>,

    /// Where throwaway trees are made.
    /// Defaults under the temp directory, KEYED BY PROCESS: two runners sharing a work directory both start
    /// at `run-1` and wipe each other's tree, which reads as a candidate whose files did not land.
    #[arg(long)]
    work_dir: Option<PathBuf>,

    /// Commands a check is allowed to name, one per `--allow`.
    /// An allow-list rather than a shell string, as for egress: the input comes from an agent.
    /// `--allow 'cargo test'` permits `cargo test` and anything after it; it does not permit `rm`.
    #[arg(long = "allow")]
    allow: Vec<String>,

    /// Seconds any single check may take before it is killed.
    #[arg(long, default_value = "300")]
    timeout: u64,

    /// A FILE holding the bearer token callers must present. Never a value.
    /// A path, so the secret never lands in argv where `ps` shows it. Optional on loopback; REQUIRED
    /// anywhere else, because an allowed command runs somebody else's code.
    #[arg(long)]
    token_file: Option<PathBuf>,

    /// Extra `KEY=VALUE` environment for the check command, repeatable.
    /// The check runs with a CLEARED environment (so an agent-authored check cannot read the runner's
    /// credentials); this is the operator's controlled exception, e.g. `UV_CACHE_DIR` so a candidate does
    /// not re-download its toolchain from a cold cache and time out. Operator input, never the agent's.
    #[arg(long = "check-env")]
    check_env: Vec<String>,

    /// How many checks may run at once. The rest wait here rather than fight the machine running them.
    /// Measured (`record-store`, shared `CARGO_TARGET_DIR`, 12 cores): 8 to 64 concurrent `cargo build`s of
    /// distinct candidates ran in 7-10s FLAT, and capping each build's `--jobs` made it slower; cargo's
    /// lock system on the shared target dir already arbitrates. This default is a generous safety ceiling
    /// (each waiting connection is one thread with a real stack), not a throttle the compile step needs.
    #[arg(long, default_value_t = 256)]
    max_concurrent: usize,
}

/// A bounded number of checks may run at once (`--max-concurrent`); the rest block here. A panicking
/// handler still releases its permit: `PermitGuard`'s `Drop` runs on unwind.
struct Permits {
    available: Mutex<usize>,
    freed: Condvar,
}

impl Permits {
    fn new(max: usize) -> Self {
        Self { available: Mutex::new(max), freed: Condvar::new() }
    }

    fn acquire(&self) -> PermitGuard<'_> {
        let mut n = self.available.lock().unwrap();
        while *n == 0 {
            n = self.freed.wait(n).unwrap();
        }
        *n -= 1;
        PermitGuard { permits: self }
    }
}

struct PermitGuard<'a> {
    permits: &'a Permits,
}

impl Drop for PermitGuard<'_> {
    fn drop(&mut self) {
        *self.permits.available.lock().unwrap() += 1;
        self.permits.freed.notify_one();
    }
}

/// One thing to check about a candidate.
#[derive(Debug, Deserialize)]
struct Check {
    id: String,
    /// A `required` check is the GATE; anything else only contributes to the score, which lets a run
    /// measure progress it cannot yet accept.
    #[serde(default)]
    required: bool,
    #[serde(default = "one")]
    weight: u32,
    /// The command, already split. Not a shell string: every parser of shell strings eventually runs
    /// something its author did not intend.
    command: Vec<String>,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Deserialize)]
struct FileChange {
    path: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct Request {
    #[serde(default)]
    candidate: String,

    /// The commit these changes are against, used ONLY as a cache key.
    /// A base tree is identical for every candidate in a generation; named by its commit id it is written
    /// once and reused, and as a content address it needs no invalidation logic.
    #[serde(default)]
    base_commit: String,

    /// The whole base tree. Sent when the runner does not have `base_commit` cached, omitted otherwise.
    /// This keeps the runner free of NATS: the caller can already read the object store, so this process
    /// needs no credentials, bucket name or git implementation.
    #[serde(default)]
    base_tree: Vec<FileChange>,

    #[serde(default)]
    changes: Vec<FileChange>,
    checks: Vec<Check>,
}

#[derive(Debug, Serialize)]
struct Result1 {
    id: String,
    required: bool,
    weight: u32,
    passed: bool,
    /// Milliseconds: a check that passes slowly is a different fact from one that passes.
    took_ms: u64,
    /// The tail of what it said, bounded so a screaming test suite cannot become the response.
    detail: String,
}

/// What the runner says when it cannot proceed without the tree.
#[derive(Debug, Serialize)]
struct NeedTree {
    need_base_tree: bool,
    base_commit: String,
    detail: String,
}

#[derive(Debug, Serialize)]
struct Report {
    candidate: String,
    accepted: bool,
    score: u32,
    passed: usize,
    total: usize,
    results: Vec<Result1>,
}

fn safe_path(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.split('/').any(|s| s.is_empty() || s == ".." || s == ".")
        && !p.contains('\0')
}

/// Prefix match on the split argv, so `--allow 'cargo test'` permits `cargo test -p thing` and refuses
/// `cargo publish`.
fn permitted(allow: &[Vec<String>], cmd: &[String]) -> bool {
    allow.iter().any(|a| a.len() <= cmd.len() && cmd[..a.len()] == a[..])
}

/// Copy the base tree. Shallow and boring on purpose: `cp -a` handles symlinks, permissions and large
/// directories better than anything worth writing here.
fn materialise(base: &Path, into: &Path) -> Result<()> {
    if into.exists() {
        std::fs::remove_dir_all(into).ok();
    }
    std::fs::create_dir_all(into.parent().unwrap_or(into))?;
    let ok = Command::new("cp")
        .arg("-a")
        .arg(base)
        .arg(into)
        .status()
        .context("copying the base tree")?
        .success();
    if !ok {
        bail!("could not copy {} to {}", base.display(), into.display());
    }
    Ok(())
}

fn apply(into: &Path, changes: &[FileChange]) -> Result<()> {
    for c in changes {
        if !safe_path(&c.path) {
            bail!("{:?} is not a path inside the tree", c.path);
        }
        let full = into.join(&c.path);
        if let Some(dir) = full.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&full, c.content.as_bytes())
            .with_context(|| format!("writing {}", full.display()))?;
    }
    Ok(())
}

fn run_check(
    dir: &Path,
    check: &Check,
    allow: &[Vec<String>],
    timeout: u64,
    extra_env: &[(String, String)],
) -> Result1 {
    let started = Instant::now();
    let refuse = |detail: String| Result1 {
        id: check.id.clone(),
        required: check.required,
        weight: check.weight.max(1),
        passed: false,
        took_ms: started.elapsed().as_millis() as u64,
        detail,
    };

    if check.command.is_empty() {
        return refuse("no command".into());
    }
    if !permitted(allow, &check.command) {
        // Refused as a FAILED check rather than an error, so one bad check does not discard the others'
        // results and the report says which one.
        return refuse(format!("`{}` is not on this runner's allow-list", check.command.join(" ")));
    }

    let mut cmd = Command::new(&check.command[0]);
    cmd.args(&check.command[1..])
        .current_dir(dir)
        // A check inherits nothing it was not given: an agent-authored check would see whatever credentials
        // the runner holds.
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()))
        .env("HOME", dir.to_string_lossy().to_string())
        .env("CARGO_TERM_COLOR", "never")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Operator-provided env, applied last so it can point a tool at a warm shared cache. Not agent input.
    for (k, v) in extra_env {
        cmd.env(k, v);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return refuse(format!("could not start: {e}")),
    };

    // Polled rather than blocking, so a hung check (a plausible bad candidate) is killed instead of
    // holding the runner forever.
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return refuse(format!("killed after {timeout}s"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return refuse(format!("waiting on it: {e}")),
        }
    }

    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return refuse(format!("reading its output: {e}")),
    };
    let mut detail = String::from_utf8_lossy(&out.stderr).to_string();
    if detail.trim().is_empty() {
        detail = String::from_utf8_lossy(&out.stdout).to_string();
    }
    // The TAIL, because a failing test suite says the useful part last.
    let detail: String = detail.chars().rev().take(600).collect::<String>().chars().rev().collect();

    Result1 {
        id: check.id.clone(),
        required: check.required,
        weight: check.weight.max(1),
        passed: out.status.success(),
        took_ms: started.elapsed().as_millis() as u64,
        detail: detail.trim().to_string(),
    }
}

fn cache_of(args: &Args, commit: &str) -> PathBuf {
    work_root(args).join("bases").join(commit)
}

fn work_root(args: &Args) -> PathBuf {
    args.work_dir
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join(format!("comp-checks-{}", std::process::id())))
}

/// Make sure the base for this request is on disk, and say where.
/// Three ways, in order of preference:
/// Disk is a MATERIALISATION: the authority is the object store the caller read from; this is a
/// scratch copy because a compiler cannot read a KV bucket.
fn ensure_base(args: &Args, req: &Request) -> Result<PathBuf, NeedTree> {
    if !req.base_commit.is_empty() {
        let cached = cache_of(args, &req.base_commit);
        if cached.is_dir() {
            return Ok(cached);
        }
        if !req.base_tree.is_empty() {
            if let Err(e) = write_tree(&cached, &req.base_tree) {
                return Err(NeedTree {
                    need_base_tree: true,
                    base_commit: req.base_commit.clone(),
                    detail: format!("could not cache the tree: {e}"),
                });
            }
            return Ok(cached);
        }
        // Asked for rather than guessed at: silently falling back to `--base` would evaluate a candidate
        // against the WRONG TREE and report a confident score.
        return Err(NeedTree {
            need_base_tree: true,
            base_commit: req.base_commit.clone(),
            detail: "this runner has not seen that commit; post `base_tree` with it".into(),
        });
    }
    match &args.base {
        Some(b) => Ok(b.clone()),
        None => Err(NeedTree {
            need_base_tree: true,
            base_commit: String::new(),
            detail: "no `base_commit` given and this runner was started without --base".into(),
        }),
    }
}

fn write_tree(into: &Path, files: &[FileChange]) -> Result<()> {
    // A partially written cache is worse than none: the next request would believe it and evaluate
    // against half a repository. Written beside and renamed, so it appears complete or not at all.
    let staging = into.with_extension("partial");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    apply(&staging, files)?;
    if let Some(parent) = into.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_dir_all(into);
    std::fs::rename(&staging, into)?;
    Ok(())
}

fn evaluate(
    args: &Args,
    allow: &[Vec<String>],
    req: &Request,
    run_id: &str,
    base_lock: &Mutex<()>,
) -> Result<std::result::Result<Report, NeedTree>> {
    // `write_tree` stages a new commit's base at a FIXED, commit-keyed path, so two concurrent first-time
    // requests for the same new commit would race. Established commits take this lock only for an
    // `is_dir` check; the checks themselves run after it is released.
    let base = {
        let _guard = base_lock.lock().unwrap();
        match ensure_base(args, req) {
            Ok(b) => b,
            Err(need) => return Ok(Err(need)),
        }
    };

    let work = work_root(args).join(run_id);
    materialise(&base, &work)?;
    apply(&work, &req.changes)?;

    let extra_env: Vec<(String, String)> = args
        .check_env
        .iter()
        .filter_map(|e| e.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect();
    let results: Vec<Result1> =
        req.checks.iter().map(|c| run_check(&work, c, allow, args.timeout, &extra_env)).collect();

    // The RUN tree is thrown away; the cached BASE is not. Debris must not poison the next candidate, and
    // kept run trees fill the disk overnight.
    let _ = std::fs::remove_dir_all(&work);

    Ok(Ok(report_of(&req.candidate, results)))
}

fn report_of(candidate: &str, results: Vec<Result1>) -> Report {
    // The gate: every REQUIRED check passed. No required checks means accepted; that is the caller's
    // choice to make.
    let accepted = results.iter().filter(|r| r.required).all(|r| r.passed);

    // The score: the weighted fraction of ALL checks, required or not, so a generation where nothing
    // passes yet has something to select on.
    let total_weight: u32 = results.iter().map(|r| r.weight).sum();
    let won: u32 = results.iter().filter(|r| r.passed).map(|r| r.weight).sum();
    let score = (won * 1000).checked_div(total_weight).unwrap_or(0);

    Report {
        candidate: candidate.to_string(),
        accepted,
        score,
        passed: results.iter().filter(|r| r.passed).count(),
        total: results.len(),
        results,
    }
}

fn respond(mut stream: TcpStream, status: u16, body: &str) {
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    );
    let _ = stream.flush();
}

/// Is this address one only this machine can reach?
/// Parsed rather than string-matched: `::1` and `localhost:8099` are loopback, `0.0.0.0` is not, and a
/// substring test for "127." would pass `10.0.127.4` and fail `::1`. A name that does not resolve is
/// NOT loopback: the safe side of an unknown demands the token.
fn is_loopback(addr: &str) -> bool {
    use std::net::ToSocketAddrs;
    match addr.to_socket_addrs() {
        Ok(mut it) => {
            let addrs: Vec<_> = it.by_ref().collect();
            !addrs.is_empty() && addrs.iter().all(|a| a.ip().is_loopback())
        }
        Err(_) => false,
    }
}

fn bearer(head: &[u8]) -> Option<String> {
    String::from_utf8_lossy(head).lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        if !k.eq_ignore_ascii_case("authorization") {
            return None;
        }
        let v = v.trim();
        // RFC 7235 §2.1: the scheme name is case-insensitive; rejecting `bearer x` reads as a wrong token.
        let (scheme, rest) = v.split_once(' ')?;
        scheme.eq_ignore_ascii_case("bearer").then(|| rest.trim().to_string())
    })
}

/// Compare in time that does not depend on where the mismatch is.
/// `==` on a String returns at the first differing byte, leaking the matching prefix length to anything
/// that can time the response; this is the comparison that stands between a caller and running
/// commands.
fn token_matches(expected: &str, given: &str) -> bool {
    let (a, b) = (expected.as_bytes(), given.as_bytes());
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(1));
    }
    diff == 0
}

fn serve(args: Args) -> Result<()> {
    let allow: Vec<Vec<String>> =
        args.allow.iter().map(|a| a.split_whitespace().map(str::to_string).collect()).collect();
    if allow.is_empty() {
        // Refusing to start beats starting useless: a runner with no allow-list fails every check, which
        // reads like every candidate being bad.
        bail!("no --allow given; this runner would refuse every check");
    }
    if let Some(b) = &args.base {
        if !b.is_dir() {
            bail!("--base {} is not a directory", b.display());
        }
    }

    let token = match &args.token_file {
        Some(p) => {
            let t = std::fs::read_to_string(p)
                .with_context(|| format!("reading --token-file {}", p.display()))?
                .trim()
                .to_string();
            if t.is_empty() {
                bail!("--token-file {} is empty", p.display());
            }
            Some(t)
        }
        None => None,
    };
    // The refusal, not a warning: a runner that printed "this is insecure" and listened anyway would be
    // found in that state months later.
    if token.is_none() && !is_loopback(&args.addr) {
        bail!(
            "refusing to listen on {} without --token-file.\n\
             \n\
             --allow bounds the COMMAND, not the tree it runs over, and `cargo test` on a tree \
             an agent wrote runs that tree's build.rs. On loopback the loopback is the boundary; \
             off it, there has to be one.\n\
             \n\
             Make a token and point both ends at it:\n\
             \x20 head -c 32 /dev/urandom | base64 > ~/.comp-secrets/checks\n\
             \x20 comp-checks --addr {} --token-file ~/.comp-secrets/checks …\n\
             \x20 comp-goalrun --checks-url http://<this-host>:<port>/check \
             --checks-token-file ~/.comp-secrets/checks …",
            args.addr,
            args.addr
        );
    }

    let listener =
        TcpListener::bind(&args.addr).with_context(|| format!("binding {}", args.addr))?;
    eprintln!(
        "comp-checks: listening on http://{} | base {} | {} allowed command(s) | {}s timeout",
        args.addr,
        args.base
            .as_ref()
            .map(|b| b.display().to_string())
            .unwrap_or_else(|| "(sent per request)".into()),
        allow.len(),
        args.timeout
    );
    // Said at startup: "is this one authenticated" is what a person asks about a runner they did not
    // start.
    eprintln!(
        "comp-checks: auth {}",
        if token.is_some() { "bearer token required" } else { "none (loopback only)" }
    );

    // Each connection is handled on its own thread, bounded by `permits` (see its doc for why that bound
    // is generous). `base_lock` serialises only the cheap "is this commit's base on disk" step; the
    // expensive work runs fully concurrently. `thread::scope` lets threads borrow `args`/`allow`/`token`.
    let permits = Permits::new(args.max_concurrent);
    let base_lock = Mutex::new(());
    let seq = AtomicU64::new(0);
    thread::scope(|scope| {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let run_id = format!("run-{}", seq.fetch_add(1, Ordering::Relaxed));
            let permit = permits.acquire();
            let args = &args;
            let allow = &allow;
            let token = &token;
            let base_lock = &base_lock;
            scope.spawn(move || {
                let _permit = permit; // held for the life of this request; released on return or panic
                handle_connection(stream, args, allow, token, run_id, base_lock);
            });
        }
    });
    Ok(())
}

/// One request, start to finish, on its own thread (see `serve`): read it, check its token, evaluate
/// it, answer it.
fn handle_connection(
    mut stream: TcpStream,
    args: &Args,
    allow: &[Vec<String>],
    token: &Option<String>,
    run_id: String,
    base_lock: &Mutex<()>,
) {
    // Read the request. A WASM guest STREAMS its body, chunked with no content-length; a reader that only
    // understands content-length waits for a close that never comes while the caller waits for the
    // response, which reads as the runner hanging.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut want: Option<usize> = None;
    let mut chunked = false;
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if want.is_none() && !chunked {
                    if let Some(pos) = find_headers_end(&buf) {
                        let head = &buf[..pos];
                        chunked = is_chunked(head);
                        if !chunked {
                            want = content_length(head).map(|len| pos + len);
                        }
                    }
                }
                if let Some(w) = want {
                    if buf.len() >= w {
                        break;
                    }
                }
                if chunked {
                    if let Some(pos) = find_headers_end(&buf) {
                        if chunk_body_complete(&buf[pos..]) {
                            break;
                        }
                    }
                }
            }
            Err(_) => break,
        }
    }

    let Some(pos) = find_headers_end(&buf) else {
        respond(stream, 400, r#"{"error":"no headers"}"#);
        return;
    };
    if let Some(expected) = &token {
        if !bearer(&buf[..pos]).is_some_and(|g| token_matches(expected, &g)) {
            // The peer, because WHERE a rejected call came from tells a misconfigured runner of your own from a
            // stranger.
            eprintln!(
                "comp-checks: rejected an unauthenticated request from {}",
                stream.peer_addr().map(|a| a.to_string()).unwrap_or_else(|_| "?".into())
            );
            respond(
                stream,
                401,
                r#"{"error":"this runner requires a bearer token; see --token-file"}"#,
            );
            return;
        }
    }
    let raw = &buf[pos..];
    let decoded;
    let body: &[u8] = if chunked {
        decoded = dechunk(raw);
        &decoded
    } else {
        raw
    };
    let req: Request = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => {
            respond(stream, 400, &format!(r#"{{"error":"bad request: {e}"}}"#));
            return;
        }
    };

    match evaluate(args, allow, &req, &run_id, base_lock) {
        // 409: the runner cannot answer until it has the tree. A distinct status because "send me the base"
        // is not a failure of the candidate, and a 200 with a made-up score would be.
        Ok(Err(need)) => {
            eprintln!(
                "comp-checks: need the tree for {}",
                if need.base_commit.is_empty() { "(no commit given)" } else { &need.base_commit }
            );
            respond(stream, 409, &serde_json::to_string(&need).unwrap_or_default());
        }
        Ok(Ok(report)) => {
            eprintln!(
                "comp-checks: {} — {}/{} passed, score {}, {}",
                if report.candidate.is_empty() { "(unnamed)" } else { &report.candidate },
                report.passed,
                report.total,
                report.score,
                if report.accepted { "ACCEPTED" } else { "rejected" }
            );
            let out = serde_json::to_string(&report).unwrap_or_default();
            respond(stream, 200, &out);
        }
        Err(e) => {
            eprintln!("comp-checks: {run_id} could not be evaluated: {e:#}");
            respond(stream, 500, &format!(r#"{{"error":"{e}"}}"#));
        }
    }
}

fn find_headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

fn is_chunked(head: &[u8]) -> bool {
    String::from_utf8_lossy(head).lines().any(|l| {
        l.split_once(':').is_some_and(|(k, v)| {
            k.eq_ignore_ascii_case("transfer-encoding") && v.trim().eq_ignore_ascii_case("chunked")
        })
    })
}

fn chunk_body_complete(body: &[u8]) -> bool {
    let mut i = 0;
    while i < body.len() {
        let Some(nl) = body[i..].windows(2).position(|w| w == b"\r\n") else { return false };
        let Ok(size) = usize::from_str_radix(String::from_utf8_lossy(&body[i..i + nl]).trim(), 16)
        else {
            return false;
        };
        if size == 0 {
            return true;
        }
        i += nl + 2 + size + 2;
    }
    false
}

/// `size\r\n<bytes>\r\n` until a zero-length chunk.
fn dechunk(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let Some(nl) = body[i..].windows(2).position(|w| w == b"\r\n") else { break };
        let Ok(size) = usize::from_str_radix(String::from_utf8_lossy(&body[i..i + nl]).trim(), 16)
        else {
            break;
        };
        if size == 0 {
            break;
        }
        let start = i + nl + 2;
        let end = (start + size).min(body.len());
        out.extend_from_slice(&body[start..end]);
        i = end + 2;
    }
    out
}

fn content_length(head: &[u8]) -> Option<usize> {
    String::from_utf8_lossy(head)
        .lines()
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("content-length")))
        .and_then(|(_, v)| v.trim().parse().ok())
}

fn main() -> Result<()> {
    let mut args = Args::parse();
    if args.addr == "127.0.0.1:8face" {
        args.addr = "127.0.0.1:8099".into();
    }
    serve(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(id: &str, required: bool, weight: u32) -> Result1 {
        Result1 {
            id: id.into(),
            required,
            weight,
            passed: false,
            took_ms: 0,
            detail: String::new(),
        }
    }

    #[test]
    fn the_gate_is_the_required_checks() {
        let mut a = check("compiles", true, 1);
        let mut b = check("lints", false, 1);
        a.passed = true;
        b.passed = false;
        let r = report_of("x", vec![a, b]);
        assert!(r.accepted, "a failing OPTIONAL check must not close the gate");

        let mut a = check("compiles", true, 1);
        a.passed = false;
        let r = report_of("x", vec![a]);
        assert!(!r.accepted, "a failing required check must close it");
    }

    #[test]
    fn the_score_gives_a_gradient_the_gate_cannot() {
        let mut some = vec![check("a", true, 1), check("b", true, 1), check("c", true, 1)];
        some[0].passed = true;
        let poor = report_of("x", some);

        let mut more = vec![check("a", true, 1), check("b", true, 1), check("c", true, 1)];
        more[0].passed = true;
        more[1].passed = true;
        let better = report_of("y", more);

        assert!(!poor.accepted && !better.accepted, "neither is acceptable yet");
        assert!(
            better.score > poor.score,
            "2 of 3 must beat 1 of 3 even though both fail the gate: {} vs {}",
            better.score,
            poor.score
        );
    }

    #[test]
    fn weight_moves_the_score_and_never_the_gate() {
        let mut heavy = vec![check("big", false, 9), check("small", false, 1)];
        heavy[0].passed = true;
        let r = report_of("x", heavy);
        assert_eq!(r.score, 900);
        assert!(r.accepted, "no required checks means nothing was demanded");
    }

    #[test]
    fn an_empty_check_list_scores_zero_rather_than_dividing_by_it() {
        let r = report_of("x", vec![]);
        assert_eq!(r.score, 0);
        assert!(r.accepted, "nothing was required, so nothing failed");
    }

    #[test]
    fn only_allowed_commands_may_run() {
        let allow: Vec<Vec<String>> =
            vec![vec!["cargo".into(), "test".into()], vec!["just".into()]];
        let cmd = |s: &str| -> Vec<String> { s.split(' ').map(str::to_string).collect() };

        assert!(permitted(&allow, &cmd("cargo test")));
        assert!(permitted(&allow, &cmd("cargo test -p thing")), "arguments after are fine");
        assert!(
            permitted(&allow, &cmd("just build")),
            "a one-word allow entry permits any subcommand"
        );

        assert!(!permitted(&allow, &cmd("cargo")), "a prefix of an allowed command is not it");
        assert!(!permitted(&allow, &cmd("cargo publish")), "a sibling subcommand is not allowed");
        assert!(!permitted(&allow, &cmd("rm -rf /")));
        assert!(!permitted(&allow, &cmd("sh -c 'cargo test'")), "no shell to hide behind");
    }

    #[test]
    fn a_change_cannot_escape_the_tree() {
        for ok in ["src/lib.rs", "a/b/c.txt", ".comp/goals/x.md"] {
            assert!(safe_path(ok), "{ok} is inside");
        }
        for bad in ["", "/etc/passwd", "../out", "a/../b", "a//b", "./a"] {
            assert!(!safe_path(bad), "{bad:?} must be refused");
        }
    }
}
