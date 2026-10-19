//! "Chat message spins up a new agent, live", on THIS platform's own native lattice (platform-domain's
//! CRUD API + comp-reconciler's convergence loop), not wasmCloud/wadm/Kubernetes.
//!
//! Nothing about the agent exists in this repo beforehand. The chat trigger carries the agent's name
//! and joke; `scaffold_agent` renders a fresh component directory from a template at that point (the
//! same shape as every component, so `cargo component build` needs no special-casing) and deletes it
//! when the test is done (`ScratchComponent`'s `Drop`).
//!
//! The "chat message" is a real NATS publish on the fleet's lattice NATS, to `holon.chat.main`,
//! standing in for a future component's trigger path. Receiving it drives render, build, POST bytes to
//! `/api/components`, POST a deployment graph, then poll until comp-reconciler has converged and the
//! capability answers over the lattice's HTTP ingress.

use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use comp_fleet::fleet::{repo_root, Fleet};
use serde_json::{json, Value};

/// Parsed out of the chat trigger: what to call the agent, and its joke.
struct AgentSpec {
    name: String,
    joke: String,
}

/// The chat trigger is free text; this is the "understanding" step a real orchestrator would do with
/// an LLM, kept to a fixed pattern since an LLM call is a separate concern.
fn parse_trigger(payload: &str) -> Option<AgentSpec> {
    let lower = payload.to_lowercase();
    if !(lower.contains("create an agent") && lower.contains("joke")) {
        return None;
    }
    // "call it <name>": the name the trigger asked for.
    let name = lower
        .split("call it ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_string();
    if name.is_empty() {
        return None;
    }
    Some(AgentSpec {
        name,
        joke: "Why do programmers prefer dark mode? Because light attracts bugs!".to_string(),
    })
}

/// A component directory rendered fresh under `components/`, removed on drop. A sibling of every other
/// component so its WIT path deps and `guestio` dep resolve as theirs do.
struct ScratchComponent {
    dir: PathBuf,
}

impl Drop for ScratchComponent {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl ScratchComponent {
    /// Render the agent's source from the spec the chat trigger carried, returning a handle that
    /// deletes the directory on drop.
    fn scaffold(spec: &AgentSpec) -> Self {
        let dir = repo_root().join("components").join(&spec.name);
        assert!(
            !dir.exists(),
            "refusing to overwrite an existing component dir: {}",
            dir.display()
        );
        std::fs::create_dir_all(dir.join("src")).expect("mkdir src");
        std::fs::create_dir_all(dir.join("wit")).expect("mkdir wit");

        std::fs::write(dir.join("Cargo.toml"), cargo_toml(&spec.name)).expect("write Cargo.toml");
        std::fs::write(dir.join("wit/world.wit"), world_wit(&spec.name)).expect("write world.wit");
        std::fs::write(dir.join("src/lib.rs"), lib_rs(spec)).expect("write lib.rs");

        Self { dir }
    }

    fn build(&self) -> Vec<u8> {
        let out = Command::new("cargo")
            .current_dir(&self.dir)
            .args(["component", "build", "--release", "--target", "wasm32-wasip2"])
            .output()
            .expect("cargo component build");
        assert!(
            out.status.success(),
            "building agent failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let wasm_name = self.dir.file_name().unwrap().to_str().unwrap().replace('-', "_");
        std::fs::read(self.dir.join(format!("target/wasm32-wasip2/release/{wasm_name}.wasm")))
            .expect("reading built wasm")
    }
}

fn cargo_toml(name: &str) -> String {
    format!(
        r#"[workspace]

[package]
name = "{name}"
version = "0.1.0"
edition = "2021"
license = "MIT"
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
guestio = {{ path = "../guestio" }}
wit-bindgen-rt = {{ version = "0.41", features = ["bitflags"] }}

[package.metadata.component]
package = "{name}:agent"

[package.metadata.component.target]
path = "wit"
world = "{name}"

[package.metadata.component.target.dependencies]
"wasi:http" = {{ path = "../../wit/deps/wasi-http-0.2.0" }}
"wasi:io" = {{ path = "../../wit/deps/wasi-io-0.2.0" }}
"wasi:clocks" = {{ path = "../../wit/deps/wasi-clocks-0.2.0" }}
"wasi:random" = {{ path = "../../wit/deps/wasi-random-0.2.0" }}
"wasi:cli" = {{ path = "../../wit/deps/wasi-cli-0.2.0" }}
"#
    )
}

fn world_wit(name: &str) -> String {
    format!(
        r#"// {name}:agent — rendered live from a chat trigger by juan_live.rs; not
// committed to this repo, built and deployed once, then deleted.
package {name}:agent@0.1.0;

world {name} {{
    export wasi:http/incoming-handler@0.2.0;
}}
"#
    )
}

fn lib_rs(spec: &AgentSpec) -> String {
    let AgentSpec { name, joke } = spec;
    format!(
        r#"//! Rendered live from a chat trigger — see reconciler/tests/juan_live.rs.
#[allow(warnings)]
mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{{Fields, OutgoingBody, OutgoingResponse, ResponseOutparam}};

guestio::guest_write_all!();

struct Component;

impl Guest for Component {{
    fn handle(_request: bindings::exports::wasi::http::incoming_handler::IncomingRequest, response_out: ResponseOutparam) {{
        let body = "Hola! I am {name}. {joke}";
        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"text/plain".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(200);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {{
            let _ = write_all(&stream, body.as_bytes());
            drop(stream);
        }}
        let _ = OutgoingBody::finish(out, None);
    }}
}}

bindings::export!(Component with_types_in bindings);
"#
    )
}

/// Register + log in + upload + deploy, against the fleet's REAL platform-domain control plane
/// (`Fleet::start_with_platform`), like `slug_live.rs`'s `Api`.
struct Api {
    base: String,
    http: reqwest::blocking::Client,
    token: String,
}
impl Api {
    fn new(base: String) -> Self {
        let http =
            reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build().unwrap();
        let cred = json!({ "email": "chat@juan.test", "password": "password123" });
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
        let r = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .unwrap();
        (r.status().as_u16(), r.json().unwrap_or(Value::Null))
    }
}

/// Call the capability over the lattice: ingress HTTP -> NATS -> the agent.
fn answer_over_lattice(fleet: &Fleet, host: &str) -> Option<String> {
    let http =
        reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build().unwrap();
    let r = http
        .get(format!("http://127.0.0.1:{}/", fleet.ingress_port))
        .header("host", host)
        .send()
        .ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.text().ok().filter(|t| !t.is_empty())
}

/// Subscribe to `subject` over the fleet's lattice NATS and return a handle whose `recv` blocks for the
/// first message. Subscribes SYNCHRONOUSLY before returning, so a publish right after cannot race core
/// NATS's fire-and-forget delivery.
struct ChatTrigger {
    rx: mpsc::Receiver<String>,
}
impl ChatTrigger {
    fn recv(self, timeout: Duration) -> String {
        self.rx.recv_timeout(timeout).expect("no chat trigger message arrived in time")
    }
}
fn subscribe_chat_trigger(nats_url: &str, subject: &str) -> ChatTrigger {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    let nats_url = nats_url.to_string();
    let subject = subject.to_string();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let client = async_nats::connect(&nats_url).await.expect("connect to subscribe");
            let mut sub = client.subscribe(subject).await.expect("subscribe");
            client.flush().await.expect("flush SUB to the server");
            let _ = ready_tx.send(());
            use futures::StreamExt;
            if let Some(msg) = sub.next().await {
                let _ = tx.send(String::from_utf8_lossy(&msg.payload).to_string());
            }
        });
    });
    ready_rx.recv_timeout(Duration::from_secs(10)).expect("subscriber never attached");
    ChatTrigger { rx }
}

#[test]
fn a_chat_message_spins_up_a_live_agent_via_platform_domain_and_the_reconciler() {
    let fleet = Fleet::start_with_platform("juanlive", 1);

    // Subscribe FIRST, synchronously, so the publish below cannot race core NATS's fire-and-forget delivery.
    let nats_url = fleet.nats_url.clone();
    let chat = subscribe_chat_trigger(&nats_url, "holon.chat.main");
    std::thread::sleep(Duration::from_millis(200));

    // The "chat message", published on the fleet's lattice NATS as a real trigger would be.
    let trigger = {
        let nats_url = nats_url.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                let client = async_nats::connect(&nats_url).await.expect("connect to publish");
                client
                    .publish(
                        "holon.chat.main",
                        "Please create an agent that tells a joke, call it juan".into(),
                    )
                    .await
                    .expect("publish chat trigger");
                // `publish` only queues the frame; without a flush it can sit unsent when the thread's connection drops.
                client.flush().await.expect("flush chat trigger to the server");
            });
        })
    };

    let payload = chat.recv(Duration::from_secs(20));
    trigger.join().unwrap();
    println!("    chat trigger received over NATS: {payload:?}");

    let spec = parse_trigger(&payload).unwrap_or_else(|| {
        panic!("trigger message did not match the expected pattern: {payload:?}")
    });
    assert_eq!(spec.name, "juan");

    // On the trigger: render, build, upload, deploy, via platform-domain's actual HTTP API.
    let agent = ScratchComponent::scaffold(&spec);
    let wasm = agent.build();
    println!("    rendered + built {}.wasm ({} bytes)", spec.name, wasm.len());

    let api = Api::new(fleet.platform_url());
    api.upload(&spec.name, wasm);
    let (code, dep) = api.post(
        "/api/deployments",
        json!({ "name": spec.name, "nodes": [{"id": spec.name}], "edges": [] }),
    );
    assert_eq!(code, 201, "deploy failed: {dep}");
    let id = dep["id"].as_str().unwrap().to_string();
    println!("    deployment created: {id}");

    // Save until comp-reconciler has converged and the agent answers over the lattice.
    let host = format!("{}.chat.test", spec.name);
    let started = Instant::now();
    let deadline = started + Duration::from_secs(180);
    let mut live = false;
    while Instant::now() < deadline {
        let _ = api.post(&format!("/api/deployments/{id}/save"), json!({}));
        if answer_over_lattice(&fleet, &host).is_some() {
            live = true;
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    assert!(live, "{} never answered over the lattice\n{}", spec.name, fleet.node_log("n1"));
    println!("    {} went live in {:?}", spec.name, started.elapsed());

    let answer = answer_over_lattice(&fleet, &host).expect("no answer");
    println!("    over the lattice: {answer}");
    assert!(
        answer.contains(&spec.name) || answer.to_lowercase().contains("juan"),
        "unexpected response: {answer:?}"
    );
    assert!(answer.contains(&spec.joke), "not the expected joke: {answer:?}");

    println!("    a chat message rendered, built, and spun up a live agent component, over the real lattice — created here, not shipped in this repo");
}
