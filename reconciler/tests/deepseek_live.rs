//! `openai-provider`, live against the real DeepSeek API.
//!
//! The whole claim: DeepSeek needs zero provider code, because it speaks the
//! OpenAI chat-completions contract already — only `wasi:config` changes
//! (`openai:base-url` to `https://api.deepseek.com`, `openai:model` to
//! `deepseek-flash`; see `fixtures/llm-deepseek-live.yaml`). This is the one
//! check that doesn't take that on faith:
//!
//!   llm-probe (wasm) -> llm:inference -> openai-provider (wasm)
//!     -> wasi:http/outgoing-handler -> egress allow-list -> api.deepseek.com
//!
//! **Live and `#[ignore]`d.** It spends real money on a real account. Put the
//! key in `~/.comp-secrets/deepseek` (nothing else — no quotes, no trailing
//! newline needed, it's trimmed) and run:
//!
//!   cargo test --release --test deepseek_live -- --ignored --nocapture
//!
//! The key never appears in this file, in the fixture, or in any test output:
//! it is read from disk by `comp-reconciler-stub` (the `@path` convention in
//! `stub.rs`, curl's own idiom), and only the model's ANSWER is ever printed.

use std::time::{Duration, Instant};

use comp_reconciler::fleet::{free_port, repo_root, Fleet};
use serde_json::Value;

fn artifacts() -> Vec<String> {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let mut out = Vec::new();
    for (id, file) in [("gate", "llm_probe.wasm"), ("llm", "openai_provider.wasm")] {
        let p = dir.join(file);
        assert!(p.exists(), "missing {} — run `cargo xtask build --force`", p.display());
        out.push(format!("{id}={}", p.display()));
    }
    out
}

fn key_path() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".comp-secrets/deepseek")
}

#[test]
#[ignore = "spends real money on a real DeepSeek account; run with --ignored --nocapture"]
fn a_completion_comes_back_from_the_real_deepseek_api() {
    let key_path = key_path();
    assert!(
        key_path.exists(),
        "need {} — put the DeepSeek API key there (nothing else in the file)",
        key_path.display()
    );
    let _ = free_port();

    let fleet = Fleet::start_with_secrets(
        "deepseek",
        &[repo_root().join("fixtures/llm-deepseek-live.yaml").to_str().unwrap()],
        &artifacts(),
        &[format!("vault://acme/deepseek=@{}", key_path.display())],
    );

    let http =
        reqwest::blocking::Client::builder().timeout(Duration::from_secs(60)).build().unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let answer: Value = loop {
        let r = http
            .get(format!(
                "http://127.0.0.1:{}/chat?q=Reply+with+exactly+one+word:+PONG",
                fleet.ingress_port
            ))
            .header("host", "llm.acme.test")
            .send();
        if let Ok(r) = r {
            let status = r.status();
            let text = r.text().unwrap_or_default();
            if status.is_success() {
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    break v;
                }
            } else if Instant::now() > deadline {
                panic!("DeepSeek answered with {status}: {text}");
            }
        }
        assert!(
            Instant::now() < deadline,
            "no answer came back\n--- node ---\n{}\n--- reconciler ---\n{}",
            fleet.node_log("n1"),
            fleet.reconciler_log()
        );
        std::thread::sleep(Duration::from_millis(500));
    };

    let text = answer["text"].as_str().unwrap_or_default();
    assert!(
        text.to_ascii_uppercase().contains("PONG"),
        "the model's answer did not survive the round trip: {answer}"
    );

    println!("\n  llm-probe -> openai-provider -> https://api.deepseek.com (deepseek-flash)");
    println!("  answered: {:?}\n", text.trim());
}
