//! `git:forge` end to end: a component opens a pull request.
//!
//! Six HTTP calls in a fixed order, and the order is a claim: the branch is
//! created LAST, so a failure partway through cannot leave an empty branch in
//! somebody's repository. Unreferenced blobs and trees are invisible and get
//! collected; a stray branch is litter a person has to explain.
//!
//! Nothing about that is checkable without a forge that records what it was
//! asked. So the forge here is a stand-in speaking GitHub's JSON, and the
//! assertions are about the SEQUENCE, the `base_tree` (without which the commit
//! deletes every file nobody touched), the base64 encoding, and the token — which
//! must arrive from the vault and can push code if it leaks.
//!
//! A real repository is deliberately not used. It costs a token with write
//! access, it leaves branches behind, and it cannot be asked what headers it
//! received — which is the assertion that matters most here.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use comp_reconciler::fleet::{free_port, repo_root, Fleet};
use serde_json::{json, Value};

mod harness;
use harness::read_chunked;

/// The token the vault holds. It is in no manifest and no config map.
const TOKEN: &str = "ghp-test-only-from-the-vault";
const BASE_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const COMMIT_SHA: &str = "cccccccccccccccccccccccccccccccccccccccc";

/// One request the stand-in forge saw.
#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    authorization: String,
    user_agent: String,
    body: Value,
}

type Log = Arc<Mutex<Vec<Seen>>>;

/// A forge that speaks GitHub's JSON and remembers everything.
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

            let (mut authorization, mut user_agent) = (String::new(), String::new());
            let (mut length, mut chunked) = (None::<usize>, false);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
                // The NAME is matched case-insensitively; the VALUE is kept as
                // sent. Lowercasing the value would also lowercase the token and
                // let a mangled one compare equal to the real one.
                if let Some((n, v)) = line.split_once(':') {
                    let (n, v) = (n.trim().to_ascii_lowercase(), v.trim().to_string());
                    match n.as_str() {
                        "authorization" => authorization = v,
                        "user-agent" => user_agent = v,
                        "content-length" => length = v.parse().ok(),
                        "transfer-encoding" => chunked = v.eq_ignore_ascii_case("chunked"),
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

            // Answer as GitHub would, per endpoint.
            let (status, answer) = if method == "GET" && path.contains("/git/ref/heads/") {
                (200, json!({ "object": { "sha": BASE_SHA, "type": "commit" } }))
            } else if path.ends_with("/git/blobs") {
                // A distinct sha per blob, so the tree call can be checked for
                // having sent the right number of distinct entries.
                let n = sink.lock().unwrap().len();
                (201, json!({ "sha": format!("b{n:039}") }))
            } else if path.ends_with("/git/trees") {
                (201, json!({ "sha": "tttttttttttttttttttttttttttttttttttttttt" }))
            } else if path.ends_with("/git/commits") {
                (201, json!({ "sha": COMMIT_SHA }))
            } else if path.ends_with("/git/refs") {
                (201, json!({ "ref": "refs/heads/x", "object": { "sha": COMMIT_SHA } }))
            } else if path.ends_with("/pulls") {
                (
                    201,
                    json!({ "number": 42, "html_url": "https://forge.test/acme/widgets/pull/42" }),
                )
            } else {
                (404, json!({ "message": "Not Found" }))
            };

            sink.lock().unwrap().push(Seen { method, path, authorization, user_agent, body });

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
    for (id, file) in [("gate", "forge_probe.wasm"), ("forge", "github_forge.wasm")] {
        let p = dir.join(file);
        assert!(p.exists(), "missing {} — run `just build`", p.display());
        out.push(format!("{id}={}", p.display()));
    }
    out
}

fn spec_for(port: u16) -> std::path::PathBuf {
    let src = repo_root().join("fixtures/git-forge.yaml");
    let yaml = std::fs::read_to_string(&src).unwrap().replace("FORGE_PORT", &port.to_string());
    let out = std::env::temp_dir().join(format!("comp-git-forge-{port}.yaml"));
    std::fs::write(&out, yaml).unwrap();
    out
}

struct Probe {
    port: u16,
    http: reqwest::blocking::Client,
}

impl Probe {
    fn call(&self, method: reqwest::Method, path: &str, body: String) -> Value {
        let r = self
            .http
            .request(method, format!("http://127.0.0.1:{}{path}", self.port))
            .header("host", "forge.acme.test")
            .body(body)
            .send()
            .expect("the probe should answer");
        let (status, text) = (r.status(), r.text().unwrap_or_default());
        serde_json::from_str(&text)
            .unwrap_or_else(|_| Value::String(format!("HTTP {status}: {text}")))
    }
}

fn wait_for_probe(fleet: &Fleet) -> Probe {
    let probe = Probe {
        port: fleet.ingress_port,
        http: reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap(),
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    while std::time::Instant::now() < deadline {
        if let Ok(r) = probe
            .http
            .get(format!("http://127.0.0.1:{}/", probe.port))
            .header("host", "forge.acme.test")
            .send()
        {
            if r.status().is_success() {
                return probe;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!(
        "the forge app never served\n--- node ---\n{}\n--- reconciler ---\n{}",
        fleet.node_log("n1"),
        fleet.reconciler_log()
    );
}

#[test]
fn a_component_opens_a_pull_request_in_an_order_that_leaves_no_litter() {
    let port = free_port();
    let seen = stand_in_forge(port);

    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let spec = spec_for(port);
    let fleet = Fleet::start_with_secrets(
        "forge",
        &[spec.to_str().unwrap()],
        &artifacts(),
        &[format!("vault://acme/forge={TOKEN}")],
    );
    let probe = wait_for_probe(&fleet);

    // The base every candidate in a generation must be judged against.
    let r = probe.call(reqwest::Method::GET, "/base", String::new());
    assert_eq!(r["base"], json!(BASE_SHA), "reading the base ref failed: {r}");

    // --- the proposal -------------------------------------------------------
    let r = probe.call(
        reqwest::Method::POST,
        "/propose",
        json!({
            "branch": "swarm/attempt-3",
            "title": "Cache the thing",
            "body": "Won generation 2 with 9/11 checks.",
            "message": "perf: cache the thing",
            "changes": [
                { "path": "src/lib.rs", "content": "fn main() { /* \"quoted\" */ }\n" },
                { "path": "docs/note.md", "content": "# note\n" },
            ],
        })
        .to_string(),
    );
    assert_eq!(r["number"], json!(42), "the pull request did not open: {r}");
    assert_eq!(r["commit"], json!(COMMIT_SHA), "the commit sha did not come back: {r}");
    assert_eq!(r["url"], json!("https://forge.test/acme/widgets/pull/42"));

    let calls = seen.lock().unwrap().clone();

    // Every call carried the vault's token, and a user-agent — GitHub refuses a
    // request without one outright, which would be a confusing 403 to debug.
    for c in &calls {
        assert_eq!(
            c.authorization,
            format!("Bearer {TOKEN}"),
            "{} {} did not carry the vault's token",
            c.method,
            c.path
        );
        assert!(!c.user_agent.is_empty(), "{} {} sent no user-agent", c.method, c.path);
    }

    // --- the order, which is the claim --------------------------------------
    let order: Vec<String> = calls
        .iter()
        .skip(1) // the /base call above
        .map(|c| {
            let tail = c.path.rsplit_once('/').map(|(_, t)| t).unwrap_or(&c.path);
            format!("{} {tail}", c.method)
        })
        .collect();
    assert_eq!(
        order,
        vec![
            "GET main",
            "POST blobs",
            "POST blobs",
            "POST trees",
            "POST commits",
            "POST refs",
            "POST pulls"
        ],
        "the sequence is the design: one blob per file, and the BRANCH LAST so a \
         failure partway cannot leave an empty branch behind. Got {order:?}"
    );

    let body_of = |needle: &str| -> Value {
        calls
            .iter()
            .rev()
            .find(|c| c.path.ends_with(needle))
            .map(|c| c.body.clone())
            .unwrap_or(Value::Null)
    };

    // Blobs are base64, so a file containing a quote survives the JSON.
    let blob = calls.iter().find(|c| c.path.ends_with("/git/blobs")).unwrap();
    assert_eq!(blob.body["encoding"], json!("base64"));
    let decoded = String::from_utf8(
        base64_decode(blob.body["content"].as_str().unwrap_or_default()).expect("blob is base64"),
    )
    .expect("blob is utf-8");
    assert_eq!(decoded, "fn main() { /* \"quoted\" */ }\n", "the file content did not survive");

    // The tree is laid OVER the base tree. Without this the commit deletes every
    // file nobody touched — the single most destructive way to get this wrong.
    let tree = body_of("/git/trees");
    assert_eq!(
        tree["base_tree"],
        json!(BASE_SHA),
        "no base_tree: this commit would delete the repo"
    );
    let entries = tree["tree"].as_array().cloned().unwrap_or_default();
    assert_eq!(entries.len(), 2, "one entry per changed file: {tree}");
    assert_eq!(entries[0]["mode"], json!("100644"));
    assert_eq!(entries[0]["path"], json!("src/lib.rs"));

    // The commit is parented on the base that was read, not on whatever the
    // branch happens to point at now.
    let commit = body_of("/git/commits");
    assert_eq!(commit["parents"], json!([BASE_SHA]), "wrong parent: {commit}");
    assert_eq!(commit["message"], json!("perf: cache the thing"));

    let refs = body_of("/git/refs");
    assert_eq!(refs["ref"], json!("refs/heads/swarm/attempt-3"));
    assert_eq!(refs["sha"], json!(COMMIT_SHA), "the branch must point at the new commit");

    let pull = body_of("/pulls");
    assert_eq!(pull["head"], json!("swarm/attempt-3"));
    assert_eq!(pull["base"], json!("main"), "base came from config");
    assert_eq!(pull["title"], json!("Cache the thing"));

    // --- an empty proposal is refused before anything is written ------------
    let before = seen.lock().unwrap().len();
    let r = probe.call(
        reqwest::Method::POST,
        "/propose",
        json!({ "branch": "swarm/empty", "title": "t", "message": "m", "changes": [] }).to_string(),
    );
    assert_eq!(r["error"], json!("rejected"), "a diff-less pull request must be refused: {r}");
    assert_eq!(
        seen.lock().unwrap().len(),
        before,
        "refusing an empty proposal must not touch the forge at all"
    );

    // A path leaving the repository is refused for the same reason, in the same place.
    let r = probe.call(
        reqwest::Method::POST,
        "/propose",
        json!({
            "branch": "swarm/escape", "title": "t", "message": "m",
            "changes": [{ "path": "../../etc/passwd", "content": "x" }],
        })
        .to_string(),
    );
    assert_eq!(r["error"], json!("rejected"), "a path escaping the repo must be refused: {r}");
    assert_eq!(seen.lock().unwrap().len(), before, "and must not reach the forge");

    println!("    opened a pull request: 6 calls, branch last, token from the vault");
}

/// Base64 without pulling a crate into the test workspace for one call.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in s.bytes().filter(|c| *c != b'=' && !c.is_ascii_whitespace()) {
        let v = T.iter().position(|t| *t == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}
