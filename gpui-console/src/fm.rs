//! Local AI inference via Apple's on-device Foundation Models CLI (`fm`): an
//! OpenAI-compatible HTTP server, spawned once as a child process (killed on drop)
//! and talked to over `/v1/chat/completions`.
//!
//! macOS/Apple Silicon only, so this is an optional host-specific capability.
//! `lattice.rs`'s `boot()` starts one (if `is_available()`) and shares its URL with
//! every agent via the deployment's `fm-url` config; a wasm guest dials that URL
//! itself and cannot call back into the host process.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use comp_fleet::fleet::free_port;
use serde_json::{json, Value};

/// A running `fm serve` process, killed on drop — same shape as
/// `reconciler::fleet::Kill`, so it cannot leave an orphan behind.
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

/// True only on macOS, and only if `fm` actually runs there. Callers check this
/// and skip rather than exec a binary that cannot exist.
pub fn is_available() -> bool {
    cfg!(target_os = "macos")
        && Command::new("fm").arg("available").output().is_ok_and(|o| o.status.success())
}

impl FmServer {
    /// Spawns `fm serve --port <port>` on a freshly-picked free port and blocks until
    /// `/health` answers (or `timeout` elapses). Refuses on anything but macOS.
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

    /// One-shot, non-streaming chat completion: an optional system instruction plus
    /// the user's prompt in, the reply text out. Not called from `lattice.rs`; kept
    /// as tested surface for a direct console feature.
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

    /// End-to-end against the real on-device model; slow, and skipped if `fm` isn't
    /// on this machine.
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
        // Signal 0 only checks whether the pid is still alive.
        let still_alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(!still_alive, "fm serve (pid {pid}) should have been killed on drop");
    }
}
