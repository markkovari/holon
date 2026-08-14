//! The `slug` capability as a LIVE component, verified over the lattice.
//!
//! Everything else about a capability has been a host test or a manifest string.
//! This deploys the real `slug` component (composed behind an HTTP front) to a
//! running fleet and CALLS it over NATS through the ingress — the behavior
//! exercised in the running system, which is the only place "the capability
//! works" is actually true.

use std::process::Command;
use std::time::{Duration, Instant};

use comp_reconciler::fleet::{repo_root, Fleet};
use serde_json::{json, Value};

/// Compose slug-probe + slug into one self-contained HTTP component and hand back
/// the bytes. The probe imports slug:generate; wac plug satisfies it with slug,
/// so the deployed artifact answers HTTP and does the real slugging internally.
fn composed_slug_probe() -> Vec<u8> {
    let comp = repo_root().join("components");
    let rel = comp.join("target/wasm32-wasip2/release");
    // Build both, generating the probe's bindings first (cargo-component codegen).
    for args in [
        vec!["component", "check", "--release", "-p", "slug-probe"],
        vec!["build", "--release", "--target", "wasm32-wasip2", "-p", "slug-probe", "-p", "slug"],
    ] {
        let out = Command::new("cargo").current_dir(&comp).args(&args).output().expect("cargo");
        assert!(out.status.success(), "{args:?} failed:\n{}", String::from_utf8_lossy(&out.stderr));
    }
    let composed = comp.join("target/slug_probe.composed.wasm");
    let out = Command::new("wac")
        .args(["plug"])
        .arg(rel.join("slug_probe.wasm"))
        .arg("--plug")
        .arg(rel.join("slug.wasm"))
        .arg("-o")
        .arg(&composed)
        .output()
        .expect("wac");
    assert!(out.status.success(), "wac plug failed:\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::read(&composed).expect("read composed")
}

struct Api {
    base: String,
    http: reqwest::blocking::Client,
    token: String,
}
impl Api {
    fn new(base: String) -> Self {
        let http =
            reqwest::blocking::Client::builder().timeout(Duration::from_secs(60)).build().unwrap();
        let deadline = Instant::now() + Duration::from_secs(120);
        while Instant::now() < deadline && http.get(&base).send().is_err() {
            std::thread::sleep(Duration::from_millis(500));
        }
        let cred = json!({ "email": "ada@slug.test", "password": "password123" });
        let _ = http.post(format!("{base}/api/register")).json(&cred).send();
        let v: Value = http
            .post(format!("{base}/api/login"))
            .json(&cred)
            .send()
            .unwrap()
            .json()
            .unwrap_or(Value::Null);
        let token = v["token"].as_str().unwrap_or_default().to_string();
        assert!(!token.is_empty(), "login failed: {v}");
        Self { base, http, token }
    }
    fn upload(&self, id: &str, wasm: Vec<u8>) {
        let code = self
            .http
            .post(format!("{}/api/components?id={id}", self.base))
            .bearer_auth(&self.token)
            .body(wasm)
            .send()
            .unwrap()
            .status()
            .as_u16();
        assert!(matches!(code, 200 | 201), "upload returned {code}");
    }
    fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self.http.post(format!("{}{path}", self.base)).bearer_auth(&self.token).json(&body).send().unwrap();
        (r.status().as_u16(), r.json().unwrap_or(Value::Null))
    }
}

/// Call the capability over the lattice: ingress HTTP -> NATS -> slug-probe ->
/// (internal) slug -> back. Returns the slug it produced.
fn slugify_over_lattice(fleet: &Fleet, text: &str) -> Option<String> {
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build().unwrap();
    let enc: String = url_encode(text);
    let r = http
        .get(format!("http://127.0.0.1:{}/slugify?text={enc}", fleet.ingress_port))
        .header("host", "slug.ada.test")
        .send()
        .ok()?;
    let v: Value = serde_json::from_str(&r.text().ok()?).ok()?;
    v["slug"].as_str().map(str::to_string)
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[test]
fn the_slug_capability_runs_and_behaves_over_the_lattice() {
    let wasm = composed_slug_probe();
    let fleet = Fleet::start_with_platform("sluglive", 1);
    let api = Api::new(fleet.platform_url());

    api.upload("slug", wasm);
    let (code, dep) =
        api.post("/api/deployments", json!({ "name": "slug", "nodes": [{"id": "slug"}], "edges": [] }));
    assert_eq!(code, 201, "deploy failed: {dep}");
    let id = dep["id"].as_str().unwrap().to_string();

    // Save until the capability answers over the lattice.
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut live = false;
    while Instant::now() < deadline {
        let _ = api.post(&format!("/api/deployments/{id}/save"), json!({}));
        if slugify_over_lattice(&fleet, "Hello, World!").is_some() {
            live = true;
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    assert!(live, "the slug capability never answered over the lattice\n{}", fleet.node_log("n1"));

    // The behavior, exercised in the running system over NATS.
    let cases: &[(&str, &str)] = &[
        ("Hello, World!", "hello-world"),
        ("Café Déjà Señor", "cafe-deja-senor"),
        ("  multiple   spaces  ", "multiple-spaces"),
        ("already-a-slug", "already-a-slug"),
    ];
    for (input, want) in cases {
        let got = slugify_over_lattice(&fleet, input).expect("no answer");
        println!("    over the lattice: slugify({input:?}) = {got:?}");
        assert_eq!(&got, want, "slugify({input:?}) over the lattice");
    }
    println!("    the slug capability is LIVE and behaves, over the lattice");
}
