//! The control plane under test, spawned per test.
//!
//! Extracted from `secrets.rs` when a second suite needed it (ADR-0073). Each
//! test gets its own process on its own port: nextest gives a test its own
//! process but not its own network, and two sharing a port fail only when the
//! suite runs in parallel — which is how it is normally run.

#![allow(dead_code)]

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};

/// Each test gets its own control plane on its own port. nextest gives a test its
/// own process but not its own network, and two of these sharing a port fail only
/// when the suite is run in parallel — which is how it is normally run.

pub struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

pub struct Platform {
    _dir: tempfile::TempDir,
    _child: Kill,
    /// Public so a suite can make a request the helpers do not cover — an odd
    /// header, a raw status code — without growing a method per test.
    pub http: reqwest::blocking::Client,
    port: u16,
}

impl Platform {
    pub fn start(port: u16) -> Self {
        let root = repo_root();
        let host = root.join("host/target/release/comp-host");
        let component = root.join("components/target/platform_domain.composed.wasm");
        assert!(host.exists(), "missing {} — cargo build --release in host/", host.display());
        assert!(component.exists(), "missing {} — just compose-platform", component.display());

        let dir = tempfile::tempdir().unwrap();
        // A real 32-byte key, per run. The vault seals every value with
        // ChaCha20-Poly1305 under it, so a wrong length is a startup failure rather
        // than a silently weaker secret.
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            std::process::id().hash(&mut h);
            let seed = h.finish().to_le_bytes();
            let raw: Vec<u8> = (0..32).map(|i| seed[i % 8] ^ i as u8).collect();
            base64(&raw)
        };
        let mut cmd = Command::new(host);
        cmd.current_dir(&root)
            .arg("--component")
            .arg(&component)
            .args(["--addr", &format!("127.0.0.1:{port}"), "--kv", "sqlite"])
            .arg("--sqlite-path")
            .arg(dir.path().join("kv.db"))
            .args(["--tenant", "platform", "--app", "control-plane"])
            .args(["--config", "applier-secret=s3cret"])
            .args(["--config", "ingress-suffix=sec.test"])
            .args(["--config", &format!("master-key={key}")]);
        let child = Kill(cmd.stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());

        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let me = Self { _dir: dir, _child: child, http, port };
        for _ in 0..60 {
            if me.http.get(me.url("/")).send().is_ok() {
                return me;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        panic!("the control plane never came up");
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// Register, then log in for a bearer token.
    ///
    /// Two calls because `/api/register` answers with the identity and not a
    /// session — the CLI does the same thing. Registering twice is harmless, so the
    /// result is ignored and the token always comes from the login.
    pub fn user(&self, name: &str) -> String {
        let body = json!({
            "email": format!("{name}@sec.test"),
            "password": format!("correct-horse-{name}"),
        });
        let _ = self.http.post(self.url("/api/register")).json(&body).send();
        let v: Value = self
            .http
            .post(self.url("/api/login"))
            .json(&body)
            .send()
            .unwrap()
            .json()
            .unwrap();
        v["token"].as_str().unwrap_or_else(|| panic!("no token in {v}")).to_string()
    }

    pub fn post(&self, token: &str, path: &str, body: Value) -> (u16, Value) {
        let r = self
            .http
            .post(self.url(path))
            .bearer_auth(token)
            .json(&body)
            .send()
            .unwrap();
        let code = r.status().as_u16();
        (code, r.json().unwrap_or(Value::Null))
    }

    pub fn get(&self, token: &str, path: &str) -> (u16, Value) {
        let r = self.http.get(self.url(path)).bearer_auth(token).send().unwrap();
        let code = r.status().as_u16();
        (code, r.json().unwrap_or(Value::Null))
    }
}

pub fn base64(raw: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in raw.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}


impl Platform {
    /// A call on the internal API, which takes the applier secret rather than a
    /// session — the reconciler's door, not a user's.
    pub fn post_internal(&self, path: &str, body: serde_json::Value) -> (u16, Value) {
        let r = self
            .http
            .post(self.url(path))
            .header("x-platform-secret", "s3cret")
            .json(&body)
            .send()
            .unwrap();
        let code = r.status().as_u16();
        (code, r.json().unwrap_or(Value::Null))
    }
}
