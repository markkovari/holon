//! Selection, end to end: which branch won, and what the forge was told.
//!
//! The assertion this test exists for is a NEGATIVE one. When no branch passed
//! its gate, the forge must see **no request at all** — not a rejected one, not a
//! branch created and abandoned. That is only checkable against a forge that
//! records what it was asked, behind a selector that would really have called it:
//! a component that never dialled out is indistinguishable from one that was
//! never asked to.
//!
//! The rest of the loop is scripted elsewhere. Here the branch results are
//! supplied directly, because what is under test is the RULE — and a rule fed by
//! a model would be a rule tested against whatever the model happened to produce.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use comp_reconciler::fleet::{free_port, repo_root, Fleet};
use serde_json::{json, Value};

mod harness;
use harness::read_chunked;

const TOKEN: &str = "ghp-test-only-from-the-vault";
const BASE_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const COMMIT_SHA: &str = "cccccccccccccccccccccccccccccccccccccccc";

#[derive(Clone, Debug)]
struct Seen {
    path: String,
    body: Value,
}

type Log = Arc<Mutex<Vec<Seen>>>;

/// A forge that speaks GitHub's JSON and remembers everything it was asked.
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
                (201, json!({ "number": 7, "html_url": "https://forge.test/acme/widgets/pull/7" }))
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


fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let mut out = Vec::new();
    for (id, file) in [
        ("gate", "select_probe.wasm"),
        ("select", "graph_selector.wasm"),
        ("forge", "github_forge.wasm"),
    ] {
        let p = dir.join(file);
        assert!(p.exists(), "missing {} — run `just build`", p.display());
        out.push(format!("{id}={}", p.display()));
    }
    out
}

fn spec_for(port: u16) -> std::path::PathBuf {
    let src = repo_root().join("fixtures/select.yaml");
    let yaml = std::fs::read_to_string(&src).unwrap().replace("FORGE_PORT", &port.to_string());
    let out = std::env::temp_dir().join(format!("comp-select-{port}.yaml"));
    std::fs::write(&out, yaml).unwrap();
    out
}

struct Probe {
    port: u16,
    http: reqwest::blocking::Client,
}

impl Probe {
    fn call(&self, route: &str, body: Value) -> Value {
        let r = match self
            .http
            .post(format!("http://127.0.0.1:{}{route}", self.port))
            .header("host", "select.acme.test")
            .body(body.to_string())
            .send()
        {
            Ok(r) => r,
            Err(e) => return Value::String(format!("transport: {e}")),
        };
        let (s, t) = (r.status(), r.text().unwrap_or_default());
        serde_json::from_str(&t).unwrap_or_else(|_| Value::String(format!("HTTP {s}: {t}")))
    }
}

/// One branch's result, as the driver would report it.
fn branch(name: &str, accepted: bool, score: u32, files: Value, tokens: u32) -> Value {
    json!({
        "branch": name, "accepted": accepted, "score": score,
        "digest": format!("{name}-digest"), "spent_tokens": tokens, "attempts": 2,
        "files": files,
    })
}

fn file(path: &str, content: &str) -> Value {
    json!({ "path": path, "content": content })
}

/// The first real selection, retried — not a separate readiness probe
/// (`Fleet::until`).
fn wait_for_probe(fleet: &Fleet) -> Probe {
    let probe = Probe {
        port: fleet.ingress_port,
        http: reqwest::blocking::Client::builder().timeout(Duration::from_secs(60)).build().unwrap(),
    };
    fleet.until("a selection", Duration::from_secs(120), || {
        let r = probe.call(
            "/select",
            json!({ "entries": [branch("only", true, 1000, json!([file("a", "b")]), 10)] }),
        );
        if r["winner"]["branch"] == json!("only") { Ok(()) } else { Err(r.to_string()) }
    });
    probe
}

#[test]
fn the_gate_is_the_only_way_to_a_pull_request() {
    let forge_port = free_port();
    let log = stand_in_forge(forge_port);
    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let spec = spec_for(forge_port);
    let fleet = Fleet::start_with_secrets(
        "select",
        &[spec.to_str().unwrap()],
        &artifacts(),
        &[format!("vault://acme/forge={TOKEN}")],
    );
    let probe = wait_for_probe(&fleet);

    // --- the forge is REACHABLE, so an empty log means something -------------
    // Asserted before the negative test rather than after. A forge that failed to
    // start — a missing secret, a denied egress — produces the same empty log as
    // a gate that held, and the first draft of this test passed for exactly that
    // reason.
    let alive = probe.call(
        "/land",
        json!({
            "entries": [branch("warmup", true, 1000, json!([file("src/warm.rs", "WARM")]), 10)],
            "landing": { "branch": "warmup", "base": "main", "title": "t", "body": "b", "message": "m" },
        }),
    );
    if alive["number"] != json!(7) {
        panic!("the forge is not reachable, so this test could not tell a held gate from a \
                broken deployment: {alive}\n--- node log ---\n{}", fleet.node_log("n1"));
    }

    // --- NOTHING PASSED THE GATE: the forge must not be touched --------------
    // The whole reason a swarm can be left running is that nothing it produces
    // reaches a repository without passing checks somebody wrote. A rejected
    // request would prove the rule was applied by the FORGE; an empty log proves
    // it was applied before anything was dialled.
    log.lock().unwrap().clear();
    let refused = probe.call(
        "/land",
        json!({
            "entries": [
                branch("close", false, 900, json!([file("src/lib.rs", "nearly")]), 400),
                branch("hopeless", false, 100, json!([file("src/lib.rs", "no")]), 900),
            ],
            "landing": { "branch": "candidate", "base": "main", "title": "t", "body": "b", "message": "m" },
        }),
    );
    assert_eq!(refused["error"], json!("nothing-acceptable"), "{refused}");
    assert!(
        refused["detail"].as_str().unwrap_or_default().contains("close"),
        "it must say how close the best branch got, or a human has nothing to act on: {refused}"
    );
    assert!(
        log.lock().unwrap().is_empty(),
        "the forge was contacted for a generation that passed no checks — the gate is not the \
         gate if a branch can reach a repository around it: {:?}",
        log.lock().unwrap()
    );

    // --- A WINNER: the forge gets the WINNER'S files, and nobody else's ------
    log.lock().unwrap().clear();
    let landed = probe.call(
        "/land",
        json!({
            "entries": [
                // Highest score of all — and it failed the gate, so it is not
                // a candidate on any other axis.
                branch("reckless", false, 1000, json!([file("src/lib.rs", "RECKLESS")]), 50),
                branch("sprawling", true, 800, json!([
                    file("src/lib.rs", "SPRAWLING"), file("src/b.rs", "x"), file("src/c.rs", "y"),
                ]), 50),
                branch("tight", true, 800, json!([file("src/lib.rs", "TIGHT")]), 4000),
            ],
            "landing": {
                "branch": "graph/cache", "base": "main",
                "title": "add a cache", "body": "from a generation of three",
                "message": "add a cache",
            },
        }),
    );
    if landed["number"] != json!(7) {
        panic!("the pull request did not open: {landed}\n--- node log ---\n{}", fleet.node_log("n1"));
    }

    let seen = log.lock().unwrap().clone();
    let blobs: Vec<String> = seen
        .iter()
        .filter(|s| s.path.ends_with("/git/blobs"))
        .map(|s| {
            let b64 = s.body["content"].as_str().unwrap_or_default();
            String::from_utf8_lossy(&base64_decode(b64)).into_owned()
        })
        .collect();
    assert_eq!(
        blobs,
        vec!["TIGHT".to_string()],
        "the forge received the wrong branch's work. `tight` and `sprawling` both passed at 800, \
         and the smaller change wins — `reckless` scored higher than either and failed its checks: \
         {blobs:?}"
    );

    // --- WHY IT WON TRAVELS WITH THE PULL REQUEST ---------------------------
    // A reviewer looking at one pull request cannot otherwise see that two other
    // branches tried, or what they scored.
    let pull = seen.iter().find(|s| s.path.ends_with("/pulls")).expect("no pull request opened");
    let body = pull.body["body"].as_str().unwrap_or_default().to_string();
    assert!(body.contains("tight"), "the pull request does not say which branch it came from: {body}");
    assert!(
        body.contains("smaller"),
        "nor why that branch won, which makes the selection unarguable after the fact: {body}"
    );

    // --- HERDING, WHICH NOTHING ELSE CAN SEE --------------------------------
    // Three branches that agreed look exactly like three that explored: same
    // count, same acceptance, a healthy-looking generation whose parallelism
    // bought nothing.
    let herd = probe.call(
        "/select",
        json!({
            "entries": [
                { "branch": "a", "accepted": true, "score": 1000, "digest": "same",
                  "spent_tokens": 100, "attempts": 1, "files": [file("src/lib.rs", "x")] },
                { "branch": "b", "accepted": true, "score": 1000, "digest": "same",
                  "spent_tokens": 100, "attempts": 1, "files": [file("src/lib.rs", "x")] },
                { "branch": "c", "accepted": true, "score": 1000, "digest": "same",
                  "spent_tokens": 100, "attempts": 1, "files": [file("src/lib.rs", "x")] },
            ],
        }),
    );
    assert_eq!(herd["accepted"], json!(3), "{herd}");
    assert_eq!(
        herd["distinct"],
        json!(1),
        "three branches produced one candidate and the report did not say so — a herded \
         generation is indistinguishable from a healthy one without this: {herd}"
    );
    assert_eq!(herd["spent_tokens"], json!(300), "and it cost three branches to learn one thing");

    println!(
        "    nothing acceptable -> the forge saw 0 requests; tight beat sprawling on size and \
         reckless on the gate; 3 branches, 1 distinct idea"
    );
}

/// Minimal base64, so the test can read what the forge was actually sent.
fn base64_decode(s: &str) -> Vec<u8> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0u32;
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
