//! `llm:inference` end to end, and the API key coming from the vault.
//!
//! `openai-provider` has been in the catalogue since it was built and had never
//! been deployed, linked or called by anything. Every claim about it was
//! therefore untested — that the host lets it dial out, that the composer links
//! it, that a caller gets an answer back, and above all that the API key arrives
//! from the SECRET vault rather than from a config map (ADR-0010). It read the
//! key from `wasi:config` until this test existed.
//!
//! The provider on the other end is a stand-in that speaks the OpenAI JSON
//! contract and records what it was sent. That is deliberate: a real provider
//! costs money, needs a key nobody should put in a repository, and — the part
//! that matters — cannot be asked what Authorization header it received. The
//! assertion this test exists for is exactly that one.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

use comp_reconciler::fleet::{free_port, repo_root, Fleet};
use serde_json::Value;

mod harness;
use harness::read_chunked;

/// The token the vault holds. It appears in no manifest and no config map, and
/// the test asserts the provider sent exactly this — so a key smuggled in
/// through config could not produce a pass.
const API_KEY: &str = "sk-test-only-from-the-vault";

/// What the stand-in provider saw.
struct Seen {
    authorization: String,
    body: String,
}

/// `size\r\n<bytes>\r\n` until a zero-length chunk.

/// An OpenAI-compatible endpoint that answers one request and reports it.
///
/// Hand-rolled over `TcpListener` rather than pulled from a crate: it has to
/// answer exactly one shape and report exactly one header, and a mock framework
/// would be more machinery than the thing under test.
fn stand_in_provider(port: u16) -> mpsc::Receiver<Seen> {
    let (tx, rx) = mpsc::channel();
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind the stand-in provider");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut authorization = String::new();
            let mut length: Option<usize> = None;
            let mut chunked = false;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                // Match on a lowercased copy, but keep the VALUE as it was sent —
                // lowercasing the header value would also lowercase the token and
                // make a mangled key compare equal to the real one.
                let (name, value) = match line.split_once(':') {
                    Some((n, v)) => (n.trim().to_ascii_lowercase(), v.trim().to_string()),
                    None => (String::new(), String::new()),
                };
                match name.as_str() {
                    "authorization" => authorization = value,
                    "content-length" => length = value.parse().ok(),
                    "transfer-encoding" => chunked = value.eq_ignore_ascii_case("chunked"),
                    _ => {}
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
            }
            // wasmtime's outgoing body is chunked when the guest streams it, which
            // is what `openai-provider` does — so content-length is absent and a
            // reader that only understands it sees an empty request.
            let body = if chunked {
                read_chunked(&mut reader)
            } else {
                let mut b = vec![0u8; length.unwrap_or(0)];
                let _ = std::io::Read::read_exact(&mut reader, &mut b);
                b
            };
            let _ = tx.send(Seen {
                authorization,
                body: String::from_utf8_lossy(&body).into_owned(),
            });

            let answer = serde_json::json!({
                "id": "chatcmpl-stand-in",
                "model": "gpt-4o-mini",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "a graph is a set of nodes and edges"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 9, "completion_tokens": 8, "total_tokens": 17}
            })
            .to_string();
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                    answer.len()
                )
                .as_bytes(),
            );
            let _ = stream.flush();
        }
    });
    rx
}

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let mut out = Vec::new();
    for (id, file) in [("gate", "llm_probe.wasm"), ("llm", "openai_provider.wasm")] {
        let p = dir.join(file);
        assert!(p.exists(), "missing {} — run `just build`", p.display());
        out.push(format!("{id}={}", p.display()));
    }
    out
}

fn spec_for(port: u16) -> std::path::PathBuf {
    let src = repo_root().join("fixtures/llm-secret.yaml");
    let yaml = std::fs::read_to_string(&src).unwrap().replace("OPENAI_PORT", &port.to_string());
    // Written outside the repo so a failed run leaves nothing behind.
    let out = std::env::temp_dir().join(format!("comp-llm-secret-{port}.yaml"));
    std::fs::write(&out, yaml).unwrap();
    out
}

fn ask(fleet: &Fleet, path: &str) -> Value {
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        let r = http
            .get(format!("http://127.0.0.1:{}{path}", fleet.ingress_port))
            .header("host", "llm.acme.test")
            .send();
        if let Ok(r) = r {
            if r.status().is_success() {
                let text = r.text().unwrap_or_default();
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    return v;
                }
            }
        }
        if std::time::Instant::now() > deadline {
            panic!(
                "{path} never answered\n--- node ---\n{}\n--- reconciler ---\n{}",
                fleet.node_log("n1"),
                fleet.reconciler_log()
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
fn a_completion_comes_back_and_the_key_came_from_the_vault() {
    let port = free_port();
    let seen = stand_in_provider(port);

    // The stand-in is on loopback, which the host refuses by default (ADR-0008).
    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let spec = spec_for(port);
    let fleet = Fleet::start_with_secrets(
        "llm",
        &[spec.to_str().unwrap()],
        &artifacts(),
        &[format!("vault://acme/openai={API_KEY}")],
    );

    let r = ask(&fleet, "/chat?q=what+is+a+graph");
    assert_eq!(
        r["text"],
        Value::String("a graph is a set of nodes and edges".into()),
        "the completion did not come back: {r}"
    );
    assert_eq!(r["finish"], Value::String("stop".into()), "finish reason lost: {r}");

    // --- the assertion this test exists for ---------------------------------
    let call = seen.recv_timeout(Duration::from_secs(5)).expect("the provider was never called");
    assert_eq!(
        call.authorization,
        format!("Bearer {API_KEY}"),
        "the API key did not reach the provider from the vault — it read the key \
         from `wasi:config` until this assertion existed, and a manifest is not a \
         place for a bearer token (ADR-0010)"
    );

    // And the request was the one the caller asked for, so a pass cannot come
    // from a provider that answered without being told anything.
    let body: Value = serde_json::from_str(&call.body).expect("the provider sent JSON");
    assert_eq!(body["model"], Value::String("gpt-4o-mini".into()), "config chose the model: {body}");
    assert_eq!(
        body["messages"][0]["content"],
        Value::String("what is a graph".into()),
        "the prompt did not survive the trip: {body}"
    );

    // The manifest that deployed this must not contain the token.
    let manifest = std::fs::read_to_string(&spec).unwrap();
    assert!(
        !manifest.contains(API_KEY),
        "the key is in the manifest, which is the thing ADR-0010 forbids"
    );

    println!("    a completion came back, and the key came from the vault");
}
