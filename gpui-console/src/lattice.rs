//! The non-UI half: boots a local dev lattice, and talks to platform-domain's
//! real HTTP API exactly the way `reconciler/tests/juan_live.rs` does.
//!
//! "Spawn new agent" renders a brand-new component's source from a template —
//! its own `Cargo.toml`, `wit/world.wit`, `src/lib.rs`, the same shape every
//! other component under `components/` has — builds it, uploads it, deploys
//! it, then deletes the scratch directory again. Nothing the UI creates is
//! left on disk or committed; platform-domain's own catalog/deployment
//! listing is the durable record, which is also where `refresh` reads from.
//! An earlier version of this redeployed a hand-written fixture wasm under a
//! new id, which only proved "deploy a pre-existing artifact live" — not
//! "create one" — matching the same correction `juan_live.rs` went through.
//!
//! Agents are keyed by NAME everywhere, not platform-domain's opaque
//! deployment id — the id is a ULID assigned to the deployment record, while
//! the HTTP ingress routes by `<name>.<tenant>.test` (the node id each
//! deployment names itself). An earlier version of this file used the
//! deployment id for ping routing, which only ever happened to not matter
//! because nothing had exercised the ping button end to end yet.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use comp_reconciler::fleet::{repo_root, Fleet};
use gpui::{Context, Subscription};
use serde_json::{json, Value};

/// One line of a conversation with an agent: who said it, and what.
#[derive(Clone)]
pub struct Message {
    pub from_user: bool,
    pub text: String,
}

impl Message {
    fn user(text: impl Into<String>) -> Self {
        Self { from_user: true, text: text.into() }
    }
    fn agent(text: impl Into<String>) -> Self {
        Self { from_user: false, text: text.into() }
    }
}

/// An agent in the sidebar, and its conversation. `messages` is local-only
/// state — platform-domain has no concept of a chat transcript, just a
/// deployment's name and status — so `refresh` (which re-reads the
/// deployment list every few seconds) preserves it by matching on `name`
/// rather than replacing the row outright.
#[derive(Clone)]
pub struct AgentRow {
    pub name: String,
    pub status: String,
    pub messages: Vec<Message>,
}

/// Everything booting the lattice produces, handed to the `Lattice` entity.
pub struct Boot {
    pub fleet: Fleet,
    pub base_url: String,
    pub tenant: String,
    pub token: String,
}

/// Boots a throwaway local dev lattice (comp-host + comp-reconciler +
/// platform-domain + comp-ingress), then registers/logs in a console account.
/// Blocking, run once before the window opens — same shape as
/// `juan_live.rs`'s `Api::new`.
pub fn boot() -> Boot {
    let fleet = Fleet::start_with_platform("console", 1);
    let base_url = fleet.platform_url();
    let tenant = "console".to_string();
    let email = format!("{tenant}@agents.test");
    let token = register_and_login(&base_url, &email, "password123");
    Boot { fleet, base_url, tenant, token }
}

pub struct Lattice {
    // `Option` so the app-quit hook below can `.take()` it, running `Fleet`'s
    // `Drop` (which kills comp-host/comp-reconciler/comp-ingress/nats-server)
    // BEFORE the process actually exits. A bare field here would never run
    // that `Drop` on a normal quit — the process tears down `main`'s stack
    // from the platform's own exit path, not Rust's, and every launch+quit
    // cycle would leak five orphaned processes. Verified exactly that leak
    // happening before this fix was added.
    _fleet: Option<Fleet>,
    _quit_guard: Subscription,
    base_url: String,
    tenant: String,
    token: String,
    ingress_port: u16,
    pub agents: Vec<AgentRow>,
    /// `None` = composing a new agent (the sidebar's "+ new agent" row);
    /// `Some(name)` = viewing/continuing that agent's conversation. The
    /// single source of truth for which pane the main panel renders.
    pub selected: Option<String>,
    pub status_line: String,
    pub spawning: bool,
}

impl Lattice {
    pub fn new(boot: Boot, cx: &mut Context<Self>) -> Self {
        let ingress_port = boot.fleet.ingress_port;
        let quit_guard = cx.on_app_quit(|this, _cx| {
            this._fleet.take();
            async {}
        });
        Self {
            _fleet: Some(boot.fleet),
            _quit_guard: quit_guard,
            base_url: boot.base_url,
            tenant: boot.tenant,
            token: boot.token,
            ingress_port,
            agents: Vec::new(),
            selected: None,
            status_line: "booted".to_string(),
            spawning: false,
        }
    }

    fn host_for(&self, name: &str) -> String {
        format!("{name}.{}.test", self.tenant)
    }

    /// `GET /api/deployments`, merged over the existing rows by NAME so an
    /// agent's conversation survives a refresh instead of being wiped every
    /// 3s, and so an agent still mid-build (not in platform-domain's catalog
    /// yet) isn't dropped from the sidebar while comp-reconciler catches up.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let base = self.base_url.clone();
        let token = self.token.clone();
        cx.spawn(async move |this, cx| {
            let fetched = cx
                .background_executor()
                .spawn(async move { list_deployments(&base, &token) })
                .await;
            this.update(cx, |this, cx| {
                for (name, status) in fetched {
                    if let Some(row) = this.agents.iter_mut().find(|a| a.name == name) {
                        row.status = status;
                    } else {
                        this.agents.insert(0, AgentRow { name, status, messages: Vec::new() });
                    }
                }
                if !this.spawning {
                    this.status_line = format!("{} agent(s) deployed", this.agents.len());
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Appends the typed text as a user message in `name`'s conversation,
    /// then calls it over the real HTTP ingress (ingress -> lattice ->
    /// component) and appends its reply — the same path
    /// `answer_over_lattice` exercises in `juan_live.rs`. The component only
    /// ever answers with its one fixed line regardless of what's sent; this
    /// still exercises and shows the real round trip, which is what this
    /// console is for.
    pub fn send_to_agent(&mut self, name: String, text: String, cx: &mut Context<Self>) {
        if let Some(row) = self.agents.iter_mut().find(|a| a.name == name) {
            row.messages.push(Message::user(text));
        }
        cx.notify();

        let ingress_port = self.ingress_port;
        let host = self.host_for(&name);
        cx.spawn(async move |this, cx| {
            let output =
                cx.background_executor().spawn(async move { ping(ingress_port, &host) }).await;
            this.update(cx, |this, cx| {
                if let Some(row) = this.agents.iter_mut().find(|a| a.name == name) {
                    row.messages
                        .push(Message::agent(output.unwrap_or_else(|| "(no answer)".to_string())));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The GUI's "create a new agent, live" action, driven by the free-text
    /// message typed into the console — mirrors `reconciler/tests/juan_live.rs`'s
    /// chat trigger, just arriving from a text field instead of a NATS
    /// publish. `extract_name` pulls the agent's name out of it (a bare word
    /// works too). The agent appears in the sidebar — and gets selected, so
    /// its conversation is immediately visible — the moment this is called,
    /// before anything has actually been built yet.
    ///
    /// Renders the agent's source from a template, builds it, uploads it,
    /// deploys it, polls until comp-reconciler has converged and it answers
    /// over the lattice, then deletes the scratch source directory again
    /// (platform-domain's catalog/deployment listing is the record that
    /// persists, not a directory on disk).
    pub fn spawn_agent(&mut self, message: String, cx: &mut Context<Self>) {
        if self.spawning {
            return;
        }
        let message = message.trim().to_string();
        if message.is_empty() {
            self.status_line = "type a message first".to_string();
            cx.notify();
            return;
        }
        let name = extract_name(&message);
        if name.is_empty() {
            self.status_line = "couldn't find a name in that message".to_string();
            cx.notify();
            return;
        }
        // WIT kebab-case identifiers require every dash-separated word to
        // start with a letter (found the hard way: `cargo component build`
        // rejects e.g. "agent-1" with "invalid label: dash-separated words
        // must begin with an ASCII lowercase letter" — a digit right after a
        // dash fails it). Caught here with a clear message instead of
        // surfacing as an opaque build failure from the background thread.
        if name.split('-').any(|word| word.chars().next().is_some_and(|c| !c.is_ascii_lowercase()))
        {
            self.status_line = format!(
                "{name}: each part of the name separated by a dash must start with a letter"
            );
            cx.notify();
            return;
        }

        self.spawning = true;
        self.status_line = format!("rendering + building {name}…");
        if let Some(row) = self.agents.iter_mut().find(|a| a.name == name) {
            row.status = "spawning".to_string();
            row.messages.push(Message::user(message));
        } else {
            self.agents.insert(
                0,
                AgentRow {
                    name: name.clone(),
                    status: "spawning".to_string(),
                    messages: vec![Message::user(message)],
                },
            );
        }
        self.selected = Some(name.clone());
        cx.notify();

        let base = self.base_url.clone();
        let token = self.token.clone();
        let ingress_port = self.ingress_port;
        let host = self.host_for(&name);
        let agent_name = name.clone();

        cx.spawn(async move |this, cx| {
            let result: Result<String, String> = cx
                .background_executor()
                .spawn(async move {
                    let wasm = scaffold_build_and_clean(&agent_name)?;
                    upload_component(&base, &token, &agent_name, wasm)?;
                    let dep_id = create_deployment(&base, &token, &agent_name)?;

                    let deadline = std::time::Instant::now() + Duration::from_secs(60);
                    while std::time::Instant::now() < deadline {
                        save_deployment(&base, &token, &dep_id);
                        if let Some(answer) = ping(ingress_port, &host) {
                            return Ok(answer);
                        }
                        std::thread::sleep(Duration::from_secs(2));
                    }
                    Err(format!("{agent_name} did not come up in time"))
                })
                .await;

            this.update(cx, |this, cx| {
                this.spawning = false;
                this.status_line = match &result {
                    Ok(_) => format!("{name} is live"),
                    Err(e) => e.clone(),
                };
                if let Some(row) = this.agents.iter_mut().find(|a| a.name == name) {
                    match &result {
                        Ok(answer) => {
                            row.status = "live".to_string();
                            row.messages.push(Message::agent(answer.clone()));
                        }
                        Err(e) => {
                            row.status = format!("failed: {e}");
                            row.messages.push(Message::agent(format!("failed: {e}")));
                        }
                    }
                }
                cx.notify();
            })
            .ok();

            if let Some(handle) = this.upgrade() {
                handle.update(cx, |this, cx| this.refresh(cx)).ok();
            }
        })
        .detach();
    }
}

/// Pulls the agent's name out of a free-text message — "create an agent that
/// tells a joke, call it juan" gives "juan", same as
/// `reconciler/tests/juan_live.rs`'s `parse_trigger`. A message with no
/// "call it" falls back to sanitizing the whole message as the name, so
/// typing a bare word still works exactly like the old name-only field did.
fn extract_name(message: &str) -> String {
    let lower = message.to_lowercase();
    if let Some(name) = lower
        .split("call it ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .map(|word| {
            word.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect::<String>()
        })
        .filter(|name| !name.is_empty())
    {
        return name;
    }
    message
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect::<String>()
        .to_lowercase()
}

// ---- blocking HTTP helpers, run on the background executor ----------------

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build().unwrap()
}

fn register_and_login(base: &str, email: &str, password: &str) -> String {
    let http = client();
    let cred = json!({ "email": email, "password": password });
    let _ = http.post(format!("{base}/api/register")).json(&cred).send();
    let v: Value = http
        .post(format!("{base}/api/login"))
        .json(&cred)
        .send()
        .expect("login request")
        .json()
        .unwrap_or(Value::Null);
    v["token"].as_str().unwrap_or_default().to_string()
}

/// `(name, status)` per deployment — the only two fields platform-domain
/// actually has that this console cares about; everything else (the
/// conversation) is local state `refresh` must not clobber.
fn list_deployments(base: &str, token: &str) -> Vec<(String, String)> {
    let http = client();
    let v: Value = match http.get(format!("{base}/api/deployments")).bearer_auth(token).send() {
        Ok(r) => r.json().unwrap_or(Value::Null),
        Err(_) => return Vec::new(),
    };
    v["deployments"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|row| {
            (
                row["name"].as_str().unwrap_or_default().to_string(),
                row["status"].as_str().unwrap_or("unknown").to_string(),
            )
        })
        .collect()
}

fn upload_component(base: &str, token: &str, id: &str, wasm: Vec<u8>) -> Result<(), String> {
    let http = client();
    let r = http
        .post(format!("{base}/api/components?id={id}"))
        .bearer_auth(token)
        .body(wasm)
        .send()
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("upload failed: {}", r.status()));
    }
    Ok(())
}

fn create_deployment(base: &str, token: &str, id: &str) -> Result<String, String> {
    let http = client();
    let r: Value = http
        .post(format!("{base}/api/deployments"))
        .bearer_auth(token)
        .json(&json!({ "name": id, "nodes": [{"id": id}], "edges": [] }))
        .send()
        .map_err(|e| e.to_string())?
        .json()
        .unwrap_or(Value::Null);
    r["id"].as_str().map(str::to_string).ok_or_else(|| format!("deploy failed: {r}"))
}

fn save_deployment(base: &str, token: &str, id: &str) {
    let http = client();
    let _ = http
        .post(format!("{base}/api/deployments/{id}/save"))
        .bearer_auth(token)
        .json(&json!({}))
        .send();
}

fn ping(ingress_port: u16, host: &str) -> Option<String> {
    let http = client();
    let r =
        http.get(format!("http://127.0.0.1:{ingress_port}/")).header("host", host).send().ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.text().ok().filter(|t| !t.is_empty())
}

/// Renders `components/<name>/` from a template (own `Cargo.toml`,
/// `wit/world.wit`, `src/lib.rs` — the same shape every other component has),
/// builds it, and deletes the directory again — win or lose, nothing it
/// creates survives this call. Mirrors `juan_live.rs`'s
/// `ScratchComponent::scaffold` + `build` + `Drop`.
fn scaffold_build_and_clean(name: &str) -> Result<Vec<u8>, String> {
    let dir: PathBuf = repo_root().join("components").join(name);
    if dir.exists() {
        return Err(format!("a component dir named {name} already exists — pick another name"));
    }
    let cleanup = |dir: &PathBuf| {
        let _ = std::fs::remove_dir_all(dir);
    };

    let render = || -> Result<(), String> {
        std::fs::create_dir_all(dir.join("src")).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(dir.join("wit")).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("Cargo.toml"), cargo_toml(name)).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("wit/world.wit"), world_wit(name)).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("src/lib.rs"), lib_rs(name)).map_err(|e| e.to_string())?;
        Ok(())
    };
    if let Err(e) = render() {
        cleanup(&dir);
        return Err(e);
    }

    let out = Command::new("cargo")
        .current_dir(&dir)
        .args(["component", "build", "--release", "--target", "wasm32-wasip2"])
        .output();
    let wasm = match out {
        Ok(o) if o.status.success() => {
            let wasm_name = name.replace('-', "_");
            std::fs::read(dir.join(format!("target/wasm32-wasip2/release/{wasm_name}.wasm")))
                .map_err(|e| e.to_string())
        }
        Ok(o) => Err(format!("building {name} failed:\n{}", String::from_utf8_lossy(&o.stderr))),
        Err(e) => Err(format!("cargo component build failed to run: {e}")),
    };
    cleanup(&dir);
    wasm
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
        r#"// {name}:agent — rendered live by gpui-console; not committed to this
// repo, built and deployed once, then deleted.
package {name}:agent@0.1.0;

world {name} {{
    export wasi:http/incoming-handler@0.2.0;
}}
"#
    )
}

fn lib_rs(name: &str) -> String {
    format!(
        r#"//! Rendered live from the console's "spawn new agent" action — see
//! gpui-console/src/lattice.rs.
#[allow(warnings)]
mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{{Fields, OutgoingBody, OutgoingResponse, ResponseOutparam}};

guestio::guest_write_all!();

struct Component;

impl Guest for Component {{
    fn handle(_request: bindings::exports::wasi::http::incoming_handler::IncomingRequest, response_out: ResponseOutparam) {{
        let body = "Hola! I am {name}, spawned live from the console.";
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
