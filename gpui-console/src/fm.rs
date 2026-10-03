//! Local AI inference via Apple's on-device Foundation Models CLI (`fm`,
//! `/usr/bin/fm` on this machine) — an OpenAI-compatible Chat Completions
//! HTTP server running entirely on-device: no network call, no API key, no
//! other inference provider involved. `fm serve` is spawned once as a child
//! process (killed on drop, same shape as `reconciler::fleet::Kill`) and
//! talked to over its real `/v1/chat/completions` endpoint.
//!
//! macOS/Apple Silicon only — this is Apple's on-device Foundation Model,
//! not something the Raspberry Pi this project's lattice otherwise targets
//! can run. That makes the AI-calling capability a host-specific optional
//! one, the same way this project's own native daemons (ffmpeg, Docker, a
//! filesystem watcher) already are — not something every node is assumed to
//! have, wired in only where it's actually available.
//!
//! `lattice.rs`'s `boot()` starts exactly one of these (if `is_available()`)
//! and shares its URL with every agent this session spawns, via each
//! deployment's `fm-url` config — a generated agent dials THAT URL directly
//! over its own `wasi:http/outgoing-handler`, not through this crate (a
//! wasm32 guest can't call back into the host process that's running it).

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use comp_reconciler::fleet::free_port;
use serde_json::{json, Value};

/// A running `fm serve` process, killed on drop — same shape as
/// `reconciler::fleet::Kill`, so a dropped `FmServer` cannot leave an
/// orphaned process behind the way this console's own lattice once did
/// before its app-quit hook was added.
pub struct FmServer {
    child: Child,
    base_url: String,
}

impl Drop for FmServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// True only on macOS, and only if `fm` actually runs there — `fm` is
/// Apple's on-device Foundation Models CLI and doesn't exist anywhere else
/// (the Raspberry Pi this project's lattice otherwise targets included).
/// Callers check this and skip rather than attempting to exec a binary that
/// was never going to be there — not a platform someone forgot to guard,
/// a platform this capability is simply not for.
pub fn is_available() -> bool {
    cfg!(target_os = "macos")
        && Command::new("fm").arg("available").output().is_ok_and(|o| o.status.success())
}

impl FmServer {
    /// Spawns `fm serve --port <port>` on a freshly-picked free port and
    /// blocks until `/health` answers (or `timeout` elapses). Refuses up
    /// front on anything but macOS (see `is_available`) rather than
    /// attempting to exec a binary that was never going to be there.
    pub fn start(timeout: Duration) -> Result<Self, String> {
        if !is_available() {
            return Err("fm (Apple on-device Foundation Models) is not available on this platform"
                .to_string());
        }
        let port = free_port();
        let child = Command::new("fm")
            .args(["serve", "--port", &port.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to spawn `fm serve` (is `fm` on PATH?): {e}"))?;

        let base_url = format!("http://127.0.0.1:{port}");
        let http = client();
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if http.get(format!("{base_url}/health")).send().is_ok_and(|r| r.status().is_success())
            {
                return Ok(Self { child, base_url });
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Err(format!("fm serve did not become healthy on {base_url} within {timeout:?}"))
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// One-shot, non-streaming chat completion: an optional system
    /// instruction plus the user's prompt in, the assistant's reply text
    /// out. Not called from `lattice.rs` — only a generated AGENT calls
    /// `fm serve`, via its own `wasi:http/outgoing-handler`, never this
    /// Rust API — but kept as real, working, tested surface rather than
    /// deleted: a direct "ask fm from the console itself" feature (a
    /// debug/test panel, say) would want exactly this.
    #[allow(dead_code)]
    pub fn ask(&self, instructions: Option<&str>, prompt: &str) -> Result<String, String> {
        ask(&self.base_url, instructions, prompt)
    }
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder().timeout(Duration::from_secs(2)).build().unwrap()
}

/// Free function so a caller that already knows a running `fm serve`'s URL
/// (not necessarily one this process itself spawned) can use it too.
#[allow(dead_code)]
pub fn ask(base_url: &str, instructions: Option<&str>, prompt: &str) -> Result<String, String> {
    let mut messages = Vec::new();
    if let Some(instructions) = instructions {
        messages.push(json!({ "role": "system", "content": instructions }));
    }
    messages.push(json!({ "role": "user", "content": prompt }));

    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    let body: Value = http
        .post(format!("{base_url}/v1/chat/completions"))
        .json(&json!({ "model": "system", "messages": messages, "stream": false }))
        .send()
        .map_err(|e| format!("request to fm serve failed: {e}"))?
        .json()
        .map_err(|e| format!("fm serve returned non-JSON: {e}"))?;

    body["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("unexpected response shape from fm serve: {body}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_on_non_macos() {
        if !cfg!(target_os = "macos") {
            assert!(!is_available(), "fm should never be reported available off macOS");
        }
    }

    #[test]
    fn start_refuses_cleanly_when_unavailable() {
        if is_available() {
            return; // this test is only meaningful where fm genuinely can't run
        }
        match FmServer::start(Duration::from_secs(1)) {
            Err(e) => assert!(e.contains("not available"), "unexpected error: {e}"),
            Ok(_) => panic!("expected start() to refuse when fm is unavailable"),
        }
    }

    /// A real end-to-end test against the actual on-device model — slow
    /// (model load + inference) and skipped if `fm` isn't on this machine,
    /// but this is exactly the thing worth verifying for real rather than
    /// mocking: that spawning `fm serve` and calling it actually produces a
    /// real answer, and that dropping it actually stops the process.
    #[test]
    fn fm_serve_spawns_answers_and_is_killed_on_drop() {
        if !is_available() {
            eprintln!("skipping: fm not available on this platform/machine");
            return;
        }

        let server = FmServer::start(Duration::from_secs(30)).expect("fm serve should start");
        let pid = server.child.id();

        let reply = server
            .ask(Some("Reply with one word only."), "What is the opposite of hot?")
            .expect("ask should succeed");
        assert!(!reply.trim().is_empty(), "expected a non-empty reply, got: {reply:?}");

        drop(server);
        std::thread::sleep(Duration::from_millis(300));
        // Signal 0: does not kill, only checks whether the pid is still
        // alive — the simplest "is it really dead" check available without
        // pulling in a process-inspection crate for one test.
        let still_alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(!still_alive, "fm serve (pid {pid}) should have been killed on drop");
    }
}
