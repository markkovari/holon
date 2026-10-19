//! The loop's inference running on `claude -p` instead of the Anthropic API.
//!
//! `tools/claude-shim.mjs` answers `/v1/messages` out of the Claude Code CLI, billing a subscription. The
//! claim is that this needs **no change to any component**: `anthropic-provider` reads its endpoint from
//! `wasi:config`, the swap point `mockllm.rs` uses.
//!
//! Other checks are unit tests (`codec.rs` pins the envelope, `goalrun.rs` the egress authority) that mock
//! out what has to work: the guest reaching a local endpoint through the host's egress control. This is
//! the one that doesn't:
//!
//!   llm-probe (wasm) -> llm:inference -> anthropic-provider (wasm)
//!     -> wasi:http/outgoing-handler -> egress allow-list -> shim -> `claude -p`
//!
//! **Live and `#[ignore]`d**: it spawns a real `claude -p` (CLI on PATH, logged-in session, ~10s,
//! subscription quota).
//!
//!   cargo test -p comp-reconciler --release --test claude_shim_live -- --ignored --nocapture

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use comp_fleet::fleet::{free_port, repo_root, Fleet};
use serde_json::Value;

/// The shim, as a child process that dies with the test.
struct Shim {
    child: Child,
    port: u16,
}

impl Drop for Shim {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Shim {
    fn start() -> Option<Self> {
        let port = free_port();
        let child = Command::new("node")
            .arg(repo_root().join("tools/claude-shim.mjs"))
            .env("PORT", port.to_string())
            .env("HOST", "127.0.0.1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let me = Self { child, port };

        // Wait for the listener rather than sleeping a guessed interval.
        let http =
            reqwest::blocking::Client::builder().timeout(Duration::from_secs(2)).build().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while std::time::Instant::now() < deadline {
            // Any answer means it is listening; a 404 here is a good readiness signal and costs no `claude -p`.
            if http.post(format!("http://127.0.0.1:{port}/ready")).body("{}").send().is_ok() {
                return Some(me);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        None
    }
}

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    [("gate", "llm_probe.wasm"), ("llm", "anthropic_provider.wasm")]
        .iter()
        .map(|(id, file)| {
            let p = dir.join(file);
            assert!(p.exists(), "missing {} — run `cargo xtask build --force`", p.display());
            format!("{id}={}", p.display())
        })
        .collect()
}

fn spec_for(port: u16) -> std::path::PathBuf {
    let yaml = std::fs::read_to_string(repo_root().join("fixtures/llm-claude-shim.yaml"))
        .unwrap()
        .replace("SHIM_PORT", &port.to_string());
    let out = std::env::temp_dir().join(format!("comp-claude-shim-{port}.yaml"));
    std::fs::write(&out, yaml).unwrap();
    out
}

#[test]
#[ignore = "spawns a real `claude -p`; run with --ignored --nocapture"]
fn a_completion_comes_back_through_claude_code() {
    if Command::new("claude").arg("--version").output().is_err() {
        eprintln!("SKIPPED: no `claude` on PATH — the Claude Code path is unverified");
        return;
    }
    let Some(shim) = Shim::start() else {
        eprintln!("SKIPPED: the shim did not start (is `node` on PATH?)");
        return;
    };

    // The shim is on loopback, which the host refuses by default (ADR-0008). Named here, not in the
    // fixture, so the widening is explicit.
    std::env::set_var("COMP_FLEET_ALLOW_PRIVATE_EGRESS", "1");
    let spec = spec_for(shim.port);
    let fleet = Fleet::start_with_secrets(
        "llm",
        &[spec.to_str().unwrap()],
        &artifacts(),
        // The shim ignores it, but the deployment shape must match production.
        &["vault://acme/anthropic=sk-unused-by-the-shim".to_string()],
    );

    // A question with exactly one sane answer, so a pass cannot come from echoing the prompt or boilerplate.
    let http =
        reqwest::blocking::Client::builder().timeout(Duration::from_secs(120)).build().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(300);
    let answer: Value = loop {
        let r = http
            .get(format!(
                "http://127.0.0.1:{}/chat?q=Reply+with+exactly+one+word:+PONG",
                fleet.ingress_port
            ))
            .header("host", "llm.acme.test")
            .send();
        if let Ok(r) = r {
            if r.status().is_success() {
                if let Ok(v) = serde_json::from_str::<Value>(&r.text().unwrap_or_default()) {
                    break v;
                }
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no answer came back\n--- node ---\n{}\n--- reconciler ---\n{}",
            fleet.node_log("n1"),
            fleet.reconciler_log()
        );
        std::thread::sleep(Duration::from_millis(500));
    };

    let text = answer["text"].as_str().unwrap_or_default();
    assert!(
        text.to_ascii_uppercase().contains("PONG"),
        "the model's answer did not survive the trip through the shim: {answer}"
    );
    // The provider parsed a real completion, not an error envelope that happened to carry text.
    assert_eq!(
        answer["finish"], "end_turn",
        "the finish reason did not come from the shim's response: {answer}"
    );

    println!("\n  llm-probe -> anthropic-provider -> shim -> claude -p");
    println!("  answered: {:?}\n", text.trim());
}
