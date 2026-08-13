//! `comp-checks` — run a candidate's checks, and report what passed.
//!
//! ## Why this is a process and not a component
//!
//! A gate has to run the project's own tests, and a component cannot spawn a
//! process — that is the sandbox working, not a gap to route around. So the one
//! part of the loop that genuinely needs an operating system is the one part that
//! is native, and it is reached over HTTP exactly like the database and the model
//! provider are (ADR-0008: what a component may dial is a manifest decision).
//!
//! It is deliberately dumb. It does not know what a goal is, what a branch is, or
//! which candidate is winning. It materialises files over a base directory, runs
//! commands, and reports. Everything that decides anything stays where it can be
//! tested without an OS.
//!
//! ## The check vector, not a verdict
//!
//! It returns EVERY check's result rather than a pass/fail, because the caller
//! needs both halves and they are different questions (ADR-0081):
//!
//!   * `required` checks that all passed  → the gate. May the branch be accepted?
//!   * the weighted fraction of ALL checks → the score. Which branch to extend?
//!
//! A binary gate gives no gradient in the generation where nothing passes yet,
//! which is exactly the generation a search has to make progress in. Reporting the
//! vector is what makes 3-of-11 beat 1-of-11 without asking a model anything.
//!
//! ## What it refuses
//!
//! Paths that leave the tree, and commands it was not configured to allow. A
//! candidate is written by an agent, and an agent that can name `../../etc` or run
//! an arbitrary shell string on the machine holding the fleet's credentials is a
//! remote code execution with extra steps.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
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
    ///
    /// OPTIONAL, and mostly not what you want. A directory pins this runner to a
    /// machine that already has the repository, which is the opposite of what
    /// putting the repository in blob storage was for (`vgit:store`). Prefer
    /// sending the tree: a caller that can read `vgit` reads it from the object
    /// store — which is on NATS and reachable from any node — and posts it once
    /// per base commit. See `base_commit` on the request.
    #[arg(long)]
    base: Option<PathBuf>,

    /// Where throwaway trees are made.
    ///
    /// Defaults under the temp directory, KEYED BY PROCESS: two runners on one
    /// machine sharing a work directory both start at `run-1`, and each wipes the
    /// other's tree before running its checks in it. That reads as a candidate
    /// whose files did not land — a wrong answer about somebody's code, produced
    /// by a collision that has nothing to do with them.
    #[arg(long)]
    work_dir: Option<PathBuf>,

    /// Commands a check is allowed to name, one per `--allow`.
    ///
    /// An allow-list rather than a shell string, for the same reason egress is an
    /// allow-list: the input comes from an agent, and "probably fine" is not a
    /// boundary. `--allow 'cargo test'` permits `cargo test` and anything after
    /// it; it does not permit `rm`.
    #[arg(long = "allow")]
    allow: Vec<String>,

    /// Seconds any single check may take before it is killed.
    #[arg(long, default_value = "300")]
    timeout: u64,
}

/// One thing to check about a candidate.
#[derive(Debug, Deserialize)]
struct Check {
    id: String,
    /// A `required` check is the GATE. Anything else contributes only to the
    /// score, which is what lets a run measure progress it cannot yet accept.
    #[serde(default)]
    required: bool,
    #[serde(default = "one")]
    weight: u32,
    /// The command, already split. Not a shell string: a string would have to be
    /// parsed, and every parser of shell strings eventually runs something its
    /// author did not intend.
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
    /// What the candidate is, for the report. The runner does not resolve it.
    #[serde(default)]
    candidate: String,

    /// The commit these changes are against, used ONLY as a cache key.
    ///
    /// A base tree is identical for every candidate in a generation, so sending
    /// it with each one would send the same repository twenty times. Named by its
    /// commit id, it is written to disk once and reused — and because a commit id
    /// is a content address, "is this the tree I think it is" needs no
    /// invalidation logic (ADR-0080's ids are real git ids).
    #[serde(default)]
    base_commit: String,

    /// The whole base tree. Sent when the runner does not have `base_commit`
    /// cached, and omitted when it does — which is every candidate after the
    /// first.
    ///
    /// This is how the runner stays free of NATS: whoever calls it can already
    /// read the object store, so the bytes come from there without this process
    /// needing credentials, a bucket name, or a git implementation of its own.
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
    /// Milliseconds. A check that passes slowly is a different fact from one that
    /// passes, and a score may want to know.
    took_ms: u64,
    /// The tail of what it said. Enough to act on, bounded so a screaming test
    /// suite cannot become the response.
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
    /// The GATE: every required check passed.
    accepted: bool,
    /// The SCORE, 0..=1000 milli-units: the weighted fraction that passed.
    score: u32,
    passed: usize,
    total: usize,
    results: Vec<Result1>,
}

/// A path inside the tree, and not outside it.
fn safe_path(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.split('/').any(|s| s.is_empty() || s == ".." || s == ".")
        && !p.contains('\0')
}

/// Is this command one the operator allowed?
///
/// Prefix match on the split argv, so `--allow 'cargo test'` permits
/// `cargo test -p thing` and refuses `cargo publish`.
fn permitted(allow: &[Vec<String>], cmd: &[String]) -> bool {
    allow.iter().any(|a| a.len() <= cmd.len() && cmd[..a.len()] == a[..])
}

/// Copy the base tree. Shallow and boring on purpose: `cp -a` handles symlinks,
/// permissions and large directories better than anything worth writing here.
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

fn run_check(dir: &Path, check: &Check, allow: &[Vec<String>], timeout: u64) -> Result1 {
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
        // Refused as a FAILED check rather than an error, so one bad check in a
        // list does not discard the results of the others — and so the report
        // says which one, where a 400 would just say the request was bad.
        return refuse(format!(
            "`{}` is not on this runner's allow-list",
            check.command.join(" ")
        ));
    }

    let mut cmd = Command::new(&check.command[0]);
    cmd.args(&check.command[1..])
        .current_dir(dir)
        // A check inherits nothing it was not given. An agent-authored check
        // running with the runner's environment would see whatever credentials
        // that process holds.
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()))
        .env("HOME", dir.to_string_lossy().to_string())
        .env("CARGO_TERM_COLOR", "never")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return refuse(format!("could not start: {e}")),
    };

    // Polled rather than blocking, so a check that hangs is killed instead of
    // holding the runner forever. A hung check is the normal way a bad candidate
    // fails — an infinite loop is a plausible thing to write.
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

/// Where a base tree is cached, by commit.
fn cache_of(args: &Args, commit: &str) -> PathBuf {
    work_root(args).join("bases").join(commit)
}

fn work_root(args: &Args) -> PathBuf {
    args.work_dir
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join(format!("comp-checks-{}", std::process::id())))
}

/// Make sure the base for this request is on disk, and say where.
///
/// Three ways, in order of preference:
///
///   1. a cached tree for this commit — the usual case after the first candidate;
///   2. a tree posted with the request, which is written to the cache;
///   3. the `--base` checkout, for a runner that has one.
///
/// Disk is a MATERIALISATION here, exactly as it is everywhere else in this
/// design: the authority is the object store the caller read from, and this is a
/// scratch copy that exists because a compiler cannot read a KV bucket.
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
        // Asked for rather than guessed at. A runner that silently fell back to
        // `--base` here would evaluate a candidate against the WRONG TREE and
        // report a confident score for it.
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
    // A partially written cache is worse than none: the next request would find
    // the directory, believe it, and evaluate against half a repository. Written
    // beside and renamed, so it appears complete or not at all.
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
) -> Result<std::result::Result<Report, NeedTree>> {
    let base = match ensure_base(args, req) {
        Ok(b) => b,
        Err(need) => return Ok(Err(need)),
    };

    let work = work_root(args).join(run_id);
    materialise(&base, &work)?;
    apply(&work, &req.changes)?;

    let results: Vec<Result1> =
        req.checks.iter().map(|c| run_check(&work, c, allow, args.timeout)).collect();

    // The RUN tree is thrown away; the cached BASE is not. A check that leaves
    // debris must not poison the next candidate, and keeping run trees around is
    // how a machine runs out of disk overnight.
    let _ = std::fs::remove_dir_all(&work);

    Ok(Ok(report_of(&req.candidate, results)))
}

/// Turn results into a gate and a score.
fn report_of(candidate: &str, results: Vec<Result1>) -> Report {
    // The gate: every REQUIRED check passed. A candidate with no required checks
    // is accepted, because nothing was demanded of it — that is the caller's
    // choice to make, not this runner's.
    let accepted = results.iter().filter(|r| r.required).all(|r| r.passed);

    // The score: the weighted fraction of ALL checks, required or not. This is
    // what gives a generation where nothing passes yet something to select on.
    let total_weight: u32 = results.iter().map(|r| r.weight).sum();
    let won: u32 = results.iter().filter(|r| r.passed).map(|r| r.weight).sum();
    let score = if total_weight == 0 { 0 } else { (won * 1000) / total_weight };

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

fn serve(args: Args) -> Result<()> {
    let allow: Vec<Vec<String>> = args
        .allow
        .iter()
        .map(|a| a.split_whitespace().map(str::to_string).collect())
        .collect();
    if allow.is_empty() {
        // Refusing to start beats starting useless: a runner with no allow-list
        // fails every check, which reads from the outside like every candidate
        // being bad.
        bail!("no --allow given; this runner would refuse every check");
    }
    if let Some(b) = &args.base {
        if !b.is_dir() {
            bail!("--base {} is not a directory", b.display());
        }
    }

    let listener = TcpListener::bind(&args.addr).with_context(|| format!("binding {}", args.addr))?;
    eprintln!(
        "comp-checks: listening on http://{} | base {} | {} allowed command(s) | {}s timeout",
        args.addr,
        args.base.as_ref().map(|b| b.display().to_string()).unwrap_or_else(|| "(sent per request)".into()),
        allow.len(),
        args.timeout
    );

    let mut seq: u64 = 0;
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        seq += 1;

        // Read the request. A WASM guest STREAMS its body, so it arrives chunked
        // with no content-length — and a reader that only understands
        // content-length waits for a close that never comes, because the caller is
        // waiting for the response. That deadlock reads from the outside as the
        // runner hanging, which is the least informative failure available.
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
            continue;
        };
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
                continue;
            }
        };

        let run_id = format!("run-{seq}");
        match evaluate(&args, &allow, &req, &run_id) {
            // 409: the runner cannot answer until it has the tree. A distinct
            // status because "send me the base" is not a failure of the
            // candidate, and answering 200 with a made-up score would be.
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
    Ok(())
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

/// Has the terminating zero-length chunk arrived?
fn chunk_body_complete(body: &[u8]) -> bool {
    let mut i = 0;
    while i < body.len() {
        let Some(nl) = body[i..].windows(2).position(|w| w == b"\r\n") else { return false };
        let Ok(size) = usize::from_str_radix(
            String::from_utf8_lossy(&body[i..i + nl]).trim(),
            16,
        ) else {
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
        let Ok(size) =
            usize::from_str_radix(String::from_utf8_lossy(&body[i..i + nl]).trim(), 16)
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

    /// The gate is the REQUIRED checks and nothing else.
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

    /// The score is what gives a search something to climb before anything passes
    /// the gate — the generation where a binary verdict is useless.
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

    /// Weight is what lets a caller say some checks matter more, without letting
    /// any of them decide acceptance on their own.
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

    /// The command allow-list is a boundary, not a suggestion. The input is
    /// written by an agent.
    #[test]
    fn only_allowed_commands_may_run() {
        let allow: Vec<Vec<String>> = vec![
            vec!["cargo".into(), "test".into()],
            vec!["just".into()],
        ];
        let cmd = |s: &str| -> Vec<String> { s.split(' ').map(str::to_string).collect() };

        assert!(permitted(&allow, &cmd("cargo test")));
        assert!(permitted(&allow, &cmd("cargo test -p thing")), "arguments after are fine");
        assert!(permitted(&allow, &cmd("just build")));

        assert!(!permitted(&allow, &cmd("cargo")), "a prefix of an allowed command is not it");
        assert!(!permitted(&allow, &cmd("cargo publish")), "a sibling subcommand is not allowed");
        assert!(!permitted(&allow, &cmd("rm -rf /")));
        assert!(!permitted(&allow, &cmd("sh -c 'cargo test'")), "no shell to hide behind");
    }

    /// A candidate is written by an agent, so a path that leaves the tree is the
    /// thing to refuse rather than to normalise.
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
