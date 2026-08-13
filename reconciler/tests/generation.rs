//! A generation, end to end: one goal, four branches at once, one pull request.
//!
//! This is the whole loop with nothing stubbed between its parts. A goal fans out
//! to four branches that run CONCURRENTLY; each writes a candidate with a model,
//! is judged by real commands, and repairs from what those commands actually
//! said; the four results are compared; the winner — and only the winner — is
//! proposed to a forge that records what it was asked.
//!
//! What each piece is:
//!
//! | the model | scripted, so a generation has a reproducible answer |
//! | the checks | real commands, so the feedback is not planted |
//! | the forge | a stand-in, so it can be asked what it received |
//! | the fan-out | real threads against a real fleet, because sequence is not parallelism |
//!
//! The branches are written to DISAGREE, which a single-run test cannot show:
//! one wins outright, one is hopeless, and two arrive at the same answer by
//! different routes — so the generation's `distinct` count is legitimately lower
//! than its branch count with nothing having gone wrong.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use comp_reconciler::fleet::{bin_path, free_port, repo_root, Fleet};
use comp_reconciler::generation::{fan_out, land, STRIDE};
use serde_json::{json, Value};

const TOKEN: &str = "ghp-test-only-from-the-vault";
const BASE_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const COMMIT_SHA: &str = "cccccccccccccccccccccccccccccccccccccccc";
const BASE: &str = "3333333333333333333333333333333333333333";

#[derive(Clone, Debug)]
struct Seen {
    path: String,
    body: Value,
}
type Log = Arc<Mutex<Vec<Seen>>>;

fn stand_in_forge(port: u16) -> Log {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind the stand-in forge");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut start = String::new();
            if reader.read_line(&mut start).unwrap_or(0) == 0 {
                continue;
            }
            let mut parts = start.split_whitespace();
            let method = parts.next().unwrap_or_default().to_string();
            let path = parts.next().unwrap_or_default().to_string();

            let (mut length, mut chunked) = (None::<usize>, false);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
                if let Some((n, v)) = line.split_once(':') {
                    match n.trim().to_ascii_lowercase().as_str() {
                        "content-length" => length = v.trim().parse().ok(),
                        "transfer-encoding" => chunked = v.trim().eq_ignore_ascii_case("chunked"),
                        _ => {}
                    }
                }
            }
            let raw = if chunked {
                read_chunked(&mut reader)
            } else {
                let mut b = vec![0u8; length.unwrap_or(0)];
                let _ = std::io::Read::read_exact(&mut reader, &mut b);
                b
            };
            let body: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);

            let (status, answer) = if method == "GET" && path.contains("/git/ref/heads/") {
                (200, json!({ "object": { "sha": BASE_SHA, "type": "commit" } }))
            } else if path.ends_with("/git/blobs") {
                let n = sink.lock().unwrap().len();
                (201, json!({ "sha": format!("b{n:039}") }))
            } else if path.ends_with("/git/trees") {
                (201, json!({ "sha": "tttttttttttttttttttttttttttttttttttttttt" }))
            } else if path.ends_with("/git/commits") {
                (201, json!({ "sha": COMMIT_SHA }))
            } else if path.ends_with("/git/refs") {
                (201, json!({ "ref": "refs/heads/x", "object": { "sha": COMMIT_SHA } }))
            } else if path.ends_with("/pulls") {
                (201, json!({ "number": 11, "html_url": "https://forge.test/acme/widgets/pull/11" }))
            } else {
                (404, json!({ "message": "Not Found" }))
            };
            sink.lock().unwrap().push(Seen { path, body });

            let out = answer.to_string();
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}",
                    out.len()
                )
                .as_bytes(),
            );
            let _ = stream.flush();
        }
    });
    log
}

fn read_chunked(reader: &mut BufReader<std::net::TcpStream>) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut size_line = String::new();
        if reader.read_line(&mut size_line).unwrap_or(0) == 0 {
            break;
        }
        let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        let mut chunk = vec![0u8; size];
        if std::io::Read::read_exact(reader, &mut chunk).is_err() {
            break;
        }
        out.extend_from_slice(&chunk);
        let mut crlf = String::new();
        let _ = reader.read_line(&mut crlf);
    }
    out
}

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
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let port = free_port();
        let child = Command::new(bin_path("comp-checks"))
            .args(["--addr", &format!("127.0.0.1:{port}")])
            .arg("--work-dir")
            .arg(dir.path())
            .args(["--allow", "test", "--allow", "grep", "--timeout", "30"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("comp-checks — run `cargo build --release` in reconciler/");
        let me = Self { child, port, _dir: dir };
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return me;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("comp-checks never listened");
    }
}

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let mut out = Vec::new();
    for (id, file) in [
        ("dprobe", "driver_probe.wasm"),
        ("gdriver", "agent_driver.wasm"),
        ("gagent", "agent_writer.wasm"),
        ("gllm", "mock_provider.wasm"),
        ("gchecks", "checks_runner.wasm"),
        ("sprobe", "select_probe.wasm"),
        ("gselect", "graph_selector.wasm"),
        ("gforge", "github_forge.wasm"),
    ] {
        let p = dir.join(file);
        assert!(p.exists(), "missing {} — run `just build`", p.display());
        out.push(format!("{id}={}", p.display()));
    }
    out
}

fn specs(checks_port: u16, forge_port: u16) -> Vec<std::path::PathBuf> {
    [("fixtures/gen-driver.yaml", "CHECKS_PORT", checks_port), ("fixtures/gen-select.yaml", "FORGE_PORT", forge_port)]
        .iter()
        .map(|(src, placeholder, port)| {
            let yaml = std::fs::read_to_string(repo_root().join(src))
                .unwrap()
                .replace(placeholder, &port.to_string());
            let name = src.rsplit('/').next().unwrap();
            let out = std::env::temp_dir().join(format!("comp-{checks_port}-{name}"));
            std::fs::write(&out, yaml).unwrap();
            out
        })
        .collect()
}

/// The goal every branch is given. Three checks: two required, one optional —
/// so branches that all pass the gate can still be ordered, which is the whole
/// reason the score exists alongside it.
fn plan() -> Value {
    json!({
        "text": "make the answer 42",
        "writable": ["src/lib.rs"],
        "context": [{ "path": "src/lib.rs", "content": "pub fn answer() -> u32 { 41 }" }],
        "checks": [
            { "id": "base-arrived", "required": true,  "weight": 1, "command": ["test", "-f", "README"] },
            { "id": "be-42",        "required": true,  "weight": 1, "command": ["grep", "-q", "42", "src/lib.rs"] },
            { "id": "tidy",         "required": false, "weight": 1, "command": ["grep", "-q", "tidy", "src/lib.rs"] },
        ],
        "base_commit": BASE,
        "base_tree": [
            { "path": "README", "content": "a project\n" },
            { "path": "src/lib.rs", "content": "pub fn answer() -> u32 { 41 }\n" },
        ],
        "max_attempts": 2,
        "seed": 0,
    })
}

#[test]
fn one_goal_four_branches_at_once_and_one_pull_request() {
    let checks = Checks::start();
    let forge_port = free_port();
    let log = stand_in_forge(forge_port);
    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");

    let specs = specs(checks.port, forge_port);
    let paths: Vec<&str> = specs.iter().map(|p| p.to_str().unwrap()).collect();
    let fleet = Fleet::start_with_secrets(
        "generation",
        &paths,
        &artifacts(),
        &[format!("vault://acme/forge={TOKEN}")],
    );

    let driver_url = format!("http://127.0.0.1:{}/run", fleet.ingress_port);
    let select_url = format!("http://127.0.0.1:{}/land", fleet.ingress_port);
    let timeout = Duration::from_secs(120);

    // Both apps up, proven by the first real call to each — not by a readiness
    // signal, which has gone green over a broken deployment in this repo before.
    fleet.until("both apps serving", Duration::from_secs(180), || {
        let mut warm = plan();
        warm["seed"] = json!(100);
        let r = comp_reconciler::generation::fan_out(&driver_url, "gendrive.acme.test", &warm, 1, 100, timeout);
        if r[0].note.is_empty() { Ok(()) } else { Err(r[0].note.clone()) }
    });

    // --- FOUR BRANCHES, AT ONCE ---------------------------------------------
    let started = Instant::now();
    let entries = fan_out(&driver_url, "gendrive.acme.test", &plan(), 4, 100, timeout);
    let wall = started.elapsed();

    for e in &entries {
        println!(
            "    {:<9} accepted={:<5} score={:<5} attempts={} took={:>5}ms stopped={:<10} {}",
            e.branch, e.accepted, e.score, e.attempts, e.elapsed_ms, e.stopped, e.note
        );
    }

    assert_eq!(entries.len(), 4);
    assert!(entries.iter().all(|e| e.note.is_empty()), "a branch never ran: {entries:?}");

    // Branch 3 is scripted to be hopeless, and a generation in which every branch
    // succeeds would not be testing selection at all.
    assert_eq!(
        entries.iter().filter(|e| e.accepted).count(),
        3,
        "three of four should pass the gate and one should not: {entries:?}"
    );
    assert_eq!(entries[3].stopped, "exhausted", "the hopeless branch should spend its budget");

    // --- AT ONCE, not one after another --------------------------------------
    // Concurrency is the entire reason to have branches, and the first version of
    // this assertion compared ATTEMPT COUNTS — which a deliberately sequential
    // `fan_out` passed, with an identical wall clock. The only thing that
    // distinguishes them is time: in parallel the wall clock is about the slowest
    // branch, in sequence it is about the sum.
    let total_attempts: u64 = entries.iter().map(|e| e.attempts).sum();
    let sum_alone: u64 = entries.iter().map(|e| e.elapsed_ms).sum();
    let slowest: u64 = entries.iter().map(|e| e.elapsed_ms).max().unwrap_or(0);
    let wall_ms = wall.as_millis() as u64;
    assert!(
        wall_ms < sum_alone * 3 / 4,
        "the generation took {wall_ms}ms and its branches took {sum_alone}ms between them \
         (slowest alone {slowest}ms) — that is a for-loop wearing the word `generation`: \
         {entries:?}"
    );
    println!(
        "    {total_attempts} attempts across 4 branches: {wall_ms}ms wall against {sum_alone}ms \
         of branch time (slowest alone {slowest}ms)"
    );

    // --- TWO BRANCHES AGREED, WITHOUT ANYTHING GOING WRONG ------------------
    // Branch 0 writes 42 outright; branch 2 writes 41, fails `be-42`, and repairs
    // to the same 42. Identical candidates by different routes — which is what
    // makes `distinct` legitimately lower than the branch count.
    assert_eq!(
        entries[0].digest, entries[2].digest,
        "these two were scripted to converge, and if they did not the `distinct` assertion \
         below is testing nothing: {entries:?}"
    );
    assert_eq!(entries[2].attempts, 2, "branch 2 should have needed a repair: {entries:?}");

    // --- THE WINNER, AND ONLY THE WINNER, REACHES THE FORGE -----------------
    log.lock().unwrap().clear();
    let opened = land(
        &select_url,
        "genselect.acme.test",
        &entries,
        json!({
            "branch": "graph/answer-42", "base": "main",
            "title": "make the answer 42", "body": "one goal, four branches",
            "message": "make the answer 42",
        }),
        timeout,
    )
    .expect("the selector was unreachable");
    assert_eq!(opened["number"], json!(11), "no pull request opened: {opened}");

    let seen = log.lock().unwrap().clone();
    let blobs: Vec<String> = seen
        .iter()
        .filter(|s| s.path.ends_with("/git/blobs"))
        .map(|s| String::from_utf8_lossy(&base64_decode(s.body["content"].as_str().unwrap_or_default())).into_owned())
        .collect();
    assert_eq!(blobs.len(), 1, "one file changed, one blob: {blobs:?}");
    assert!(
        blobs[0].contains("tidy"),
        "the forge got the wrong branch. Branch 1 is the only one that satisfied the OPTIONAL \
         check as well, so it alone scored 1000 — and a selector that stopped at the gate would \
         have taken whichever accepted branch came first: {blobs:?}"
    );

    let pull = seen.iter().find(|s| s.path.ends_with("/pulls")).expect("no pull request");
    let body = pull.body["body"].as_str().unwrap_or_default();
    assert!(body.contains("branch-1"), "the pull request does not say which branch won: {body}");

    println!("    branch-1 won on the optional check and opened PR #11");
}

/// Minimal base64, so the test can read what the forge was actually sent.
fn base64_decode(s: &str) -> Vec<u8> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let (mut out, mut acc, mut bits) = (Vec::new(), 0u32, 0u32);
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=') {
        let Some(v) = T.iter().position(|t| *t == c) else { continue };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// The stride is the reason branch 1's first attempt is not branch 0's second.
#[test]
fn seeds_are_spaced_further_apart_than_any_run_is_long() {
    assert!(
        STRIDE > 8,
        "attempt n of a branch uses seed+n, so a stride at or below max-attempts makes two \
         branches ask an identical question — which is the one thing a generation exists to avoid"
    );
}
