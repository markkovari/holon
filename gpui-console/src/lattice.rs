//! The non-UI half: boots a local dev lattice plus the agent runtime, and
//! talks to both.
//!
//! An agent is two things that are deliberately separate:
//!
//! * a **spec + brain** in `agent_runtime` — name, description, capabilities,
//!   triggers, model, memory, run log. Edited by writing a JSON file; the next
//!   run uses it. Nothing is rebuilt. Schedule and event triggers fire from
//!   here, whether or not anyone has the console open on that agent.
//! * a **front door** on the lattice — one generic `agent-gateway` component,
//!   uploaded once per agent under the agent's name, which forwards HTTP to the
//!   runtime. This is what makes `<name>.<tenant>.test` answer, through the
//!   real ingress -> lattice -> component path.
//!
//! The UI's transcript is driven by the runtime's event stream, not by what
//! the UI itself sent, so a run triggered by a cron schedule or another agent
//! shows up in that agent's conversation exactly like one typed here.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use agent_runtime::agent::Event;
use agent_runtime::model::LocalModel;
use agent_runtime::runtime::Pending;
use agent_runtime::spec::{validate_name, Capability};
use agent_runtime::store::{MemoryItem, RunRecord, Status, Step};
use agent_runtime::{server, AgentSpec, Config, ModelSpec, Runtime, Trigger};
use comp_reconciler::fleet::{repo_root, Fleet};
use futures::channel::mpsc;
use futures::StreamExt;
use gpui::{Context, Subscription};
use serde_json::{json, Value};

use crate::fm;

/// Where the agent runtime listens. FIXED, not picked per launch: each
/// deployment's gateway has this URL in its config and egress allow-list, and
/// deployments persist across launches. `HOLON_AGENT_RUNTIME_PORT` overrides
/// it (and then existing deployments need respawning).
const DEFAULT_RUNTIME_PORT: u16 = 18017;

/// Who a conversation line is from. `System` is a checkpoint — a trigger, a
/// tool call, an approval, a status change — interleaved with the real
/// user/agent messages.
#[derive(Clone, PartialEq, Eq)]
pub enum Kind {
    User,
    Agent,
    System,
}

#[derive(Clone)]
pub struct Message {
    pub kind: Kind,
    pub text: String,
}

impl Message {
    fn user(text: impl Into<String>) -> Self {
        Self { kind: Kind::User, text: text.into() }
    }
    fn agent(text: impl Into<String>) -> Self {
        Self { kind: Kind::Agent, text: text.into() }
    }
    fn system(text: impl Into<String>) -> Self {
        Self { kind: Kind::System, text: text.into() }
    }
}

/// An agent in the sidebar. `messages` is the console's rendering of the
/// runtime's run log (rebuilt from it on launch, extended by its events).
#[derive(Clone)]
pub struct AgentRow {
    pub name: String,
    pub description: String,
    /// The lattice deployment's status, or "no gateway" if it has none.
    pub status: String,
    pub paused: bool,
    pub capabilities: Vec<String>,
    pub triggers: Vec<String>,
    pub model: String,
    pub messages: Vec<Message>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Chat,
    Memory,
    Spec,
}

pub struct Boot {
    pub fleet: Fleet,
    pub base_url: String,
    pub tenant: String,
    pub token: String,
    pub fm: Option<fm::FmServer>,
    pub runtime: Arc<Runtime>,
    pub runtime_port: u16,
    pub runtime_dir: PathBuf,
}

/// Boots the runtime (with `fm serve` as its local model where available),
/// then the lattice, then registers/logs in a console account. Blocking, run
/// once before the window opens.
pub fn boot() -> Boot {
    let dir = persistent_state_dir();
    let runtime_dir = dir.join("runtime");
    std::fs::create_dir_all(&runtime_dir).expect("creating the runtime state dir");

    let fm = if fm::is_available() {
        match fm::FmServer::start(Duration::from_secs(20)) {
            Ok(server) => Some(server),
            Err(e) => {
                eprintln!("fm is available but failed to start; local-model agents will fail: {e}");
                None
            }
        }
    } else {
        None
    };

    let mut cfg = Config::new(&runtime_dir);
    cfg.local = LocalModel {
        base_url: fm.as_ref().map(|s| s.base_url().to_string()),
        model: "system".into(),
    };
    let runtime = Runtime::new(cfg).expect("opening the agent runtime");
    let port = std::env::var("HOLON_AGENT_RUNTIME_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_RUNTIME_PORT);
    let token = server::admin_token_in(&runtime_dir).expect("admin token");
    server::serve(runtime.clone(), &format!("127.0.0.1:{port}"), token).unwrap_or_else(|e| {
        panic!("{e} — is another console or `agent-runtime` already running on port {port}?")
    });
    runtime.start_scheduler();

    // The gateway component dials the runtime from inside the tenant's
    // sandbox, and egress is default-deny, so the OPERATOR (this process, as
    // the control plane's launcher) grants exactly that one authority to
    // every tenant — never a tenant itself. See platform-domain's
    // `default-egress` and ADR-0008.
    std::env::set_var("COMP_DEFAULT_EGRESS", format!("127.0.0.1:{port}"));
    let host_args =
        vec!["--egress".to_string(), "127.0.0.1".to_string(), "--allow-private-egress".to_string()];
    let fleet = Fleet::start_with_platform_in_dir("console", 1, dir, &host_args);
    let base_url = fleet.platform_url();
    let tenant = "console".to_string();
    let token = register_and_login(&base_url, &format!("{tenant}@agents.test"), "password123");

    Boot { fleet, base_url, tenant, token, fm, runtime, runtime_port: port, runtime_dir }
}

/// Where this console's lattice lives across launches. Fixed and outside the
/// repo, so a relaunch sees the same deployments.
fn persistent_state_dir() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME must be set");
    PathBuf::from(home).join(".holon-console-state")
}

pub struct Lattice {
    // `Option` so the app-quit hook can `.take()` them and run their `Drop`
    // (killing the lattice's child processes, `fm serve`) before the process
    // exits — a bare field's `Drop` never runs on a normal quit.
    _fleet: Option<Fleet>,
    _fm: Option<fm::FmServer>,
    _quit_guard: Subscription,
    rt: Arc<Runtime>,
    runtime_port: u16,
    pub runtime_dir: PathBuf,
    base_url: String,
    tenant: String,
    token: String,
    ingress_port: u16,
    pub agents: Vec<AgentRow>,
    pub approvals: Vec<Pending>,
    /// `None` = nothing selected; `Some(name)` = that agent's pane.
    pub selected: Option<String>,
    pub view: View,
    /// Lines for the Memory / Spec views, loaded when the view is opened.
    pub detail: Vec<String>,
    pub status_line: String,
    pub spawning: bool,
}

impl Lattice {
    pub fn new(boot: Boot, cx: &mut Context<Self>) -> Self {
        let ingress_port = boot.fleet.ingress_port;
        let quit_guard = cx.on_app_quit(|this, _cx| {
            this.rt.shutdown();
            this._fleet.take();
            this._fm.take();
            async {}
        });

        // The runtime's events arrive on a std channel and from any thread;
        // hop them onto the UI's executor.
        let rx = boot.runtime.subscribe();
        let (tx, mut events) = mpsc::unbounded::<Event>();
        std::thread::spawn(move || {
            while let Ok(ev) = rx.recv() {
                if tx.unbounded_send(ev).is_err() {
                    break;
                }
            }
        });
        cx.spawn(async move |this, cx| {
            while let Some(ev) = events.next().await {
                if this.update(cx, |this, cx| this.on_event(ev, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        let agents =
            boot.runtime.store().list().iter().map(|s| row_from(s, &boot.runtime)).collect();
        Self {
            _fleet: Some(boot.fleet),
            _fm: boot.fm,
            _quit_guard: quit_guard,
            rt: boot.runtime,
            runtime_port: boot.runtime_port,
            runtime_dir: boot.runtime_dir,
            base_url: boot.base_url,
            tenant: boot.tenant,
            token: boot.token,
            ingress_port,
            agents,
            approvals: Vec::new(),
            selected: None,
            view: View::Chat,
            detail: Vec::new(),
            status_line: "booted".to_string(),
            spawning: false,
        }
    }

    fn host_for(&self, name: &str) -> String {
        format!("{name}.{}.test", self.tenant)
    }

    fn row_mut(&mut self, name: &str) -> Option<&mut AgentRow> {
        self.agents.iter_mut().find(|a| a.name == name)
    }

    // ---- runtime events -> transcript ------------------------------------

    fn on_event(&mut self, ev: Event, cx: &mut Context<Self>) {
        match ev {
            Event::RunStarted { agent, trigger, input, .. } => {
                if let Some(row) = self.row_mut(&agent) {
                    row.messages.extend(start_messages(&trigger, &input));
                }
            }
            Event::StepDone { agent, step, .. } => {
                if let (Some(row), Some(m)) = (self.row_mut(&agent), step_message(&step)) {
                    row.messages.push(m);
                }
            }
            Event::RunFinished(rec) => {
                if let Some(row) = self.row_mut(&rec.agent) {
                    row.messages.extend(finish_messages(&rec));
                }
            }
            Event::ApprovalRequested { id, agent, tool, args } => {
                if let Some(row) = self.row_mut(&agent) {
                    row.messages.push(Message::system(format!(
                        "needs your approval: {tool} {}",
                        short(&args.to_string(), 160)
                    )));
                }
                self.approvals.push(Pending { id, agent, tool, args });
            }
            Event::ApprovalResolved { id, approved } => {
                if let Some(p) = self.approvals.iter().find(|p| p.id == id).cloned() {
                    if let Some(row) = self.row_mut(&p.agent) {
                        row.messages.push(Message::system(format!(
                            "{} {}",
                            p.tool,
                            if approved { "approved" } else { "denied" }
                        )));
                    }
                }
                self.approvals.retain(|p| p.id != id);
            }
        }
        cx.notify();
    }

    pub fn resolve_approval(&mut self, id: u64, approved: bool, cx: &mut Context<Self>) {
        self.rt.resolve_approval(id, approved);
        cx.notify();
    }

    // ---- refresh ----------------------------------------------------------

    /// Re-reads the agent list from the runtime (the source of truth for what
    /// exists) and each agent's gateway status from platform-domain, keeping
    /// every conversation by NAME.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let base = self.base_url.clone();
        let token = self.token.clone();
        cx.spawn(async move |this, cx| {
            let deployments = cx
                .background_executor()
                .spawn(async move { list_deployments(&base, &token) })
                .await;
            this.update(cx, |this, cx| {
                let specs = this.rt.store().list();
                this.agents.retain(|a| specs.iter().any(|s| s.name == a.name));
                for spec in &specs {
                    let status = deployments
                        .iter()
                        .find(|d| d.name == spec.name)
                        .map(|d| d.status.clone())
                        .unwrap_or_else(|| "no gateway".to_string());
                    match this.agents.iter_mut().find(|a| a.name == spec.name) {
                        Some(row) => {
                            let fresh = row_from(spec, &this.rt);
                            if row.status != status
                                && row.status != "spawning"
                                && !row.status.starts_with("failed")
                            {
                                row.messages.push(Message::system(format!(
                                    "status: {} → {status}",
                                    row.status
                                )));
                            }
                            if !row.status.starts_with("failed") || status != "no gateway" {
                                row.status = status;
                            }
                            row.description = fresh.description;
                            row.paused = fresh.paused;
                            row.capabilities = fresh.capabilities;
                            row.triggers = fresh.triggers;
                            row.model = fresh.model;
                        }
                        None => {
                            let mut row = row_from(spec, &this.rt);
                            row.status = status;
                            this.agents.push(row);
                        }
                    }
                }
                this.agents.sort_by(|a, b| a.name.cmp(&b.name));
                if this.selected.as_ref().is_some_and(|s| !this.agents.iter().any(|a| &a.name == s))
                {
                    this.selected = None;
                }
                if !this.spawning {
                    this.status_line = format!("{} agent(s)", this.agents.len());
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- chat -------------------------------------------------------------

    /// Sends `text` to the agent over the real HTTP ingress (ingress ->
    /// lattice -> gateway -> runtime). The transcript fills from the runtime's
    /// events — this only reports a failure the runtime never saw.
    pub fn send_to_agent(&mut self, name: String, text: String, cx: &mut Context<Self>) {
        let (port, host) = (self.ingress_port, self.host_for(&name));
        cx.spawn(async move |this, cx| {
            // An agent run can wait minutes on an approval, so no short timeout.
            let out = cx
                .background_executor()
                .spawn(async move { ping(port, &host, Some(&text), Duration::from_secs(400)) })
                .await;
            if let Err(e) = out {
                this.update(cx, |this, cx| {
                    if let Some(row) = this.row_mut(&name) {
                        row.messages.push(Message::system(format!("no reply: {e}")));
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    // ---- lifecycle --------------------------------------------------------

    pub fn validate_name(&self, name: &str) -> Result<(), String> {
        validate_name(name)?;
        if self.agents.iter().any(|a| a.name == name) {
            return Err(format!("{name} already exists — pick another name"));
        }
        Ok(())
    }

    /// Creates an agent: saves its spec in the runtime (so schedule/event
    /// triggers work immediately), then gives it a lattice front door — the
    /// shared gateway component, uploaded and deployed under its name.
    pub fn spawn_agent(&mut self, spec: AgentSpec, cx: &mut Context<Self>) {
        if self.spawning {
            return;
        }
        let name = spec.name.clone();
        if let Err(e) = self.validate_name(&name).and_then(|_| self.rt.create_agent(spec.clone())) {
            self.status_line = e;
            cx.notify();
            return;
        }
        self.spawning = true;
        self.status_line = format!("creating {name}…");
        let mut row = row_from(&spec, &self.rt);
        row.status = "spawning".into();
        row.messages.push(Message::system("agent created — giving it a lattice front door…"));
        self.agents.push(row);
        self.agents.sort_by(|a, b| a.name.cmp(&b.name));
        self.selected = Some(name.clone());
        self.view = View::Chat;
        cx.notify();

        let (base, token) = (self.base_url.clone(), self.token.clone());
        let runtime_url = format!("http://127.0.0.1:{}", self.runtime_port);
        let (ingress_port, host) = (self.ingress_port, self.host_for(&name));

        let (progress_tx, mut progress_rx) = mpsc::unbounded::<String>();
        {
            let name = name.clone();
            cx.spawn(async move |this, cx| {
                while let Some(line) = progress_rx.next().await {
                    this.update(cx, |this, cx| {
                        if let Some(row) = this.row_mut(&name) {
                            row.messages.push(Message::system(line));
                        }
                        cx.notify();
                    })
                    .ok();
                }
            })
            .detach();
        }

        cx.spawn(async move |this, cx| {
            let tx = progress_tx.clone();
            let agent = name.clone();
            let result: Result<(), String> = cx
                .background_executor()
                .spawn(async move {
                    let say = |s: &str| {
                        let _ = tx.unbounded_send(s.to_string());
                    };
                    say("preparing the gateway component…");
                    let wasm = gateway_wasm()?;
                    say("uploading…");
                    upload_component(&base, &token, &agent, wasm, &["runtime-url", "agent"])?;
                    say("deploying…");
                    let dep_id = create_deployment(&base, &token, &agent, &runtime_url)?;
                    say("waiting for comp-reconciler to converge…");
                    let deadline = std::time::Instant::now() + Duration::from_secs(60);
                    let mut last: Option<String> = None;
                    while std::time::Instant::now() < deadline {
                        let err = save_deployment(&base, &token, &dep_id);
                        if err != last {
                            if let Some(e) = &err {
                                say(e);
                            }
                            last = err;
                        }
                        if ping_ready(ingress_port, &host) {
                            return Ok(());
                        }
                        std::thread::sleep(Duration::from_secs(2));
                    }
                    Err(format!("{agent}'s gateway did not come up in time"))
                })
                .await;
            drop(progress_tx);

            this.update(cx, |this, cx| {
                this.spawning = false;
                this.status_line = match &result {
                    Ok(()) => format!("{name} is live"),
                    Err(e) => e.clone(),
                };
                if let Some(row) = this.row_mut(&name) {
                    match &result {
                        Ok(()) => {
                            row.status = "live".to_string();
                            row.messages.push(Message::system("live — answering over the lattice"));
                        }
                        Err(e) => {
                            row.status = format!("failed: {e}");
                            row.messages.push(Message::system(format!(
                                "no HTTP front door ({e}) — schedule and event triggers still work"
                            )));
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

    pub fn set_paused(&mut self, name: &str, paused: bool, cx: &mut Context<Self>) {
        match self.rt.set_paused(name, paused) {
            Ok(()) => {
                if let Some(row) = self.row_mut(name) {
                    row.paused = paused;
                    row.messages.push(Message::system(if paused { "paused" } else { "resumed" }));
                }
            }
            Err(e) => self.status_line = e,
        }
        cx.notify();
    }

    /// Removes the agent's spec, memory, runs and workspace, and its lattice
    /// deployment (which also destroys the deployment's storage — see
    /// platform-domain's `?confirm=` guard).
    pub fn delete_agent(&mut self, name: String, cx: &mut Context<Self>) {
        if let Err(e) = self.rt.delete_agent(&name) {
            self.status_line = e;
            cx.notify();
            return;
        }
        self.agents.retain(|a| a.name != name);
        self.selected = None;
        self.status_line = format!("deleted {name}");
        cx.notify();
        let (base, token) = (self.base_url.clone(), self.token.clone());
        cx.background_executor()
            .spawn(async move {
                if let Some(d) =
                    list_deployments(&base, &token).into_iter().find(|d| d.name == name)
                {
                    let _ = client()
                        .delete(format!("{base}/api/deployments/{}?confirm={}", d.id, d.name))
                        .bearer_auth(&token)
                        .send();
                }
            })
            .detach();
    }

    pub fn select(&mut self, name: String, cx: &mut Context<Self>) {
        self.selected = Some(name);
        self.view = View::Chat;
        cx.notify();
    }

    pub fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        self.view = view;
        self.detail = match (view, &self.selected) {
            (View::Memory, Some(n)) => {
                let m: Vec<MemoryItem> = self.rt.store().memories(n);
                if m.is_empty() {
                    vec!["nothing remembered yet".to_string()]
                } else {
                    m.iter().rev().map(|i| i.text.clone()).collect()
                }
            }
            (View::Spec, Some(n)) => {
                let path = self.runtime_dir.join("agents").join(format!("{n}.json"));
                let mut lines =
                    vec![format!("edit {} — the next run uses it", path.display()), String::new()];
                if let Some(s) = self.rt.store().get(n) {
                    lines.extend(
                        serde_json::to_string_pretty(&s)
                            .unwrap_or_default()
                            .lines()
                            .map(String::from),
                    );
                }
                lines
            }
            _ => Vec::new(),
        };
        cx.notify();
    }
}

// ---- transcript rendering of runtime data ----------------------------------

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// An HTTP-triggered run reads as the user's message; every other trigger as a
/// checkpoint saying what woke the agent.
fn start_messages(trigger: &str, input: &str) -> Vec<Message> {
    match trigger {
        "http" => vec![Message::user(input)],
        t => vec![Message::system(format!("woken by {t}: {}", short(input, 120)))],
    }
}

fn step_message(step: &Step) -> Option<Message> {
    match step {
        Step::Model { .. } => None,
        Step::Tool { name, args, result, error, approved } => Some(Message::system(format!(
            "{name} {}{} → {}{}",
            short(&args.to_string(), 80),
            match approved {
                Some(true) => " (approved)",
                Some(false) => " (denied)",
                None => "",
            },
            if *error { "error: " } else { "" },
            short(result, 120)
        ))),
    }
}

fn finish_messages(rec: &RunRecord) -> Vec<Message> {
    let mut v = Vec::new();
    match rec.status {
        Status::Ok => v.push(Message::agent(&rec.answer)),
        Status::Failed => v.push(Message::system(format!("run failed: {}", rec.answer))),
        Status::OverBudget => v.push(Message::system(format!("stopped: {}", rec.answer))),
    }
    v.push(Message::system(format!("{} tokens", rec.tokens_in + rec.tokens_out)));
    v
}

fn row_from(spec: &AgentSpec, rt: &Runtime) -> AgentRow {
    let mut messages = Vec::new();
    for rec in rt.store().runs(&spec.name, 8).iter().rev() {
        messages.extend(start_messages(&rec.trigger, &rec.input));
        messages.extend(rec.steps.iter().filter_map(step_message));
        messages.extend(finish_messages(rec));
    }
    AgentRow {
        name: spec.name.clone(),
        description: spec.description.clone(),
        status: "…".into(),
        paused: spec.paused,
        capabilities: spec.capabilities.iter().map(|c| c.name.clone()).collect(),
        triggers: spec
            .triggers
            .iter()
            .map(|t| match t {
                Trigger::Schedule { cron, .. } => format!("every {cron}"),
                Trigger::Event { topic } => format!("on {topic}"),
            })
            .collect(),
        model: match &spec.model {
            ModelSpec::Local => "local".into(),
            ModelSpec::OpenAi { model, .. } | ModelSpec::Anthropic { model, .. } => model.clone(),
            ModelSpec::Mock { .. } => "mock".into(),
        },
        messages,
    }
}

// ---- turning the new-agent form's text into a spec -------------------------

/// What the "+ new agent" form collects, as raw text.
#[derive(Default, Clone)]
pub struct FormInput {
    pub name: String,
    pub description: String,
    /// `;`-separated. Each: `name [@ wit-ref] [| what it does]`. A name that
    /// is a built-in tool (`http_get`, `write_file`, ...) or `agent:<other>`
    /// becomes callable; any other name is a text ability the model is told
    /// about. e.g. `http_get; fetch @ os:http/client.get | get a page`.
    pub capabilities: String,
    /// `;`-separated `cron :: task`, e.g. `*/10 * * * * :: check the feed`.
    pub schedules: String,
    /// Comma-separated event topics this agent wakes on.
    pub events: String,
    /// Comma-separated hosts `http_get` may reach.
    pub hosts: String,
    /// Blank/`local`, `anthropic:<model>`, or `openai:<base-url>|<model>`.
    pub model: String,
    /// Comma-separated sensitive tools allowed without asking.
    pub auto_approve: String,
}

fn csv(s: &str) -> Vec<String> {
    s.split([',', '\n']).map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()
}

pub fn spec_from_form(f: &FormInput) -> Result<AgentSpec, String> {
    let name: String = f
        .name
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect::<String>()
        .to_lowercase();
    validate_name(&name)?;
    let mut spec = AgentSpec::new(&name, f.description.trim());

    for entry in f.capabilities.split(';').map(str::trim).filter(|e| !e.is_empty()) {
        let (head, desc) = entry.split_once('|').map_or((entry, ""), |(h, d)| (h.trim(), d.trim()));
        let (cname, wit) = head
            .split_once('@')
            .map_or((head, None), |(n, w)| (n.trim(), Some(w.trim().to_string())));
        if cname.is_empty() {
            return Err(format!("capability `{entry}` has no name"));
        }
        if let Some(existing) = spec.capabilities.iter_mut().find(|c| c.name == cname) {
            existing.description = desc.to_string();
            existing.wit = wit;
        } else {
            spec.capabilities.push(Capability {
                name: cname.to_string(),
                description: desc.to_string(),
                wit,
            });
        }
    }
    for entry in f.schedules.split(';').map(str::trim).filter(|e| !e.is_empty()) {
        let (cron, prompt) = entry.split_once("::").ok_or_else(|| {
            format!("schedule `{entry}`: write it as `cron expression :: what to do`")
        })?;
        spec.triggers.push(Trigger::Schedule {
            cron: cron.trim().to_string(),
            prompt: prompt.trim().to_string(),
        });
    }
    for topic in csv(&f.events) {
        spec.triggers.push(Trigger::Event { topic });
    }
    spec.allow_hosts = csv(&f.hosts);
    spec.auto_approve = csv(&f.auto_approve);
    spec.model = match f.model.trim() {
        "" | "local" => ModelSpec::Local,
        m if m.starts_with("anthropic:") => ModelSpec::Anthropic {
            model: m["anthropic:".len()..].trim().to_string(),
            api_key_env: "ANTHROPIC_API_KEY".into(),
        },
        m if m.starts_with("openai:") => {
            let (url, model) = m["openai:".len()..]
                .split_once('|')
                .ok_or("model: write it as `openai:<base-url>|<model>`")?;
            ModelSpec::OpenAi {
                base_url: url.trim().to_string(),
                model: model.trim().to_string(),
                api_key_env: "OPENAI_API_KEY".into(),
            }
        }
        other => {
            return Err(format!(
                "model `{other}`: use local, anthropic:<model> or openai:<base-url>|<model>"
            ))
        }
    };
    agent_runtime::runtime::validate_spec(&spec)?;
    Ok(spec)
}

// ---- blocking HTTP helpers, run on the background executor -----------------

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

struct Deployment {
    id: String,
    name: String,
    status: String,
}

fn list_deployments(base: &str, token: &str) -> Vec<Deployment> {
    let v: Value = match client().get(format!("{base}/api/deployments")).bearer_auth(token).send() {
        Ok(r) => r.json().unwrap_or(Value::Null),
        Err(_) => return Vec::new(),
    };
    v["deployments"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|row| Deployment {
            id: row["id"].as_str().unwrap_or_default().to_string(),
            name: row["name"].as_str().unwrap_or_default().to_string(),
            status: row["status"].as_str().unwrap_or("unknown").to_string(),
        })
        .collect()
}

/// The generic gateway, built once and reused for every agent. Rebuilt only
/// if its sources are newer than the artifact — a rebuild takes seconds and
/// is the ONLY cargo invocation spawning an agent can ever trigger.
fn gateway_wasm() -> Result<Vec<u8>, String> {
    let dir = repo_root().join("agent-runtime/gateway");
    let out = dir.join("target/wasm32-wasip2/release/agent_gateway.wasm");
    let mtime = |p: PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let newest_src = ["src/lib.rs", "wit/world.wit", "Cargo.toml"]
        .iter()
        .filter_map(|f| mtime(dir.join(f)))
        .max();
    let stale = match (mtime(out.clone()), newest_src) {
        (Some(built), Some(src)) => src > built,
        (None, _) => true,
        _ => false,
    };
    if stale {
        let o = Command::new("cargo")
            .current_dir(&dir)
            .args(["component", "build", "--release", "--target", "wasm32-wasip2"])
            .output()
            .map_err(|e| format!("cargo component build failed to run: {e}"))?;
        if !o.status.success() {
            return Err(format!(
                "building the gateway failed:\n{}",
                String::from_utf8_lossy(&o.stderr)
            ));
        }
    }
    std::fs::read(&out).map_err(|e| format!("{}: {e}", out.display()))
}

/// `config_keys` DECLARES which `wasi:config` keys the component may be given —
/// separate from, and a prerequisite for, a deployment supplying a VALUE.
fn upload_component(
    base: &str,
    token: &str,
    id: &str,
    wasm: Vec<u8>,
    config_keys: &[&str],
) -> Result<(), String> {
    let mut url = format!("{base}/api/components?id={id}");
    if !config_keys.is_empty() {
        url.push_str("&config=");
        url.push_str(&config_keys.join(","));
    }
    let r = client().post(url).bearer_auth(token).body(wasm).send().map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("upload failed: {}", r.status()));
    }
    Ok(())
}

fn create_deployment(
    base: &str,
    token: &str,
    id: &str,
    runtime_url: &str,
) -> Result<String, String> {
    let node = json!({ "id": id, "config": { "runtime-url": runtime_url, "agent": id } });
    let r: Value = client()
        .post(format!("{base}/api/deployments"))
        .bearer_auth(token)
        .json(&json!({ "name": id, "nodes": [node], "edges": [] }))
        .send()
        .map_err(|e| e.to_string())?
        .json()
        .unwrap_or(Value::Null);
    r["id"].as_str().map(str::to_string).ok_or_else(|| format!("deploy failed: {r}"))
}

/// `Some(msg)` on anything but success. A transient "not distributed yet" is
/// normal; a real refusal must not be swallowed.
fn save_deployment(base: &str, token: &str, id: &str) -> Option<String> {
    match client()
        .post(format!("{base}/api/deployments/{id}/save"))
        .bearer_auth(token)
        .json(&json!({}))
        .send()
    {
        Ok(r) if r.status().is_success() => None,
        Ok(r) => {
            let status = r.status();
            Some(format!("save: {status} {}", r.text().unwrap_or_default()))
        }
        Err(e) => Some(format!("save: transport error: {e}")),
    }
}

/// Calls the agent over the real HTTP ingress. `question` goes as `?q=`.
fn ping(
    ingress_port: u16,
    host: &str,
    question: Option<&str>,
    timeout: Duration,
) -> Result<String, String> {
    let http =
        reqwest::blocking::Client::builder().timeout(timeout).build().map_err(|e| e.to_string())?;
    let mut req = http.get(format!("http://127.0.0.1:{ingress_port}/")).header("host", host);
    if let Some(q) = question {
        req = req.query(&[("q", q)]);
    }
    let r = req.send().map_err(|e| e.to_string())?;
    let status = r.status();
    let body = r.text().unwrap_or_default();
    if status.is_success() {
        Ok(body)
    } else {
        Err(format!("{status}: {}", short(&body, 200)))
    }
}

/// Is the agent's gateway up and forwarding? Hits `/ping`, which costs no
/// model call, so polling it while converging spends nothing.
fn ping_ready(ingress_port: u16, host: &str) -> bool {
    let http = client();
    http.get(format!("http://127.0.0.1:{ingress_port}/ping"))
        .header("host", host)
        .send()
        .is_ok_and(|r| r.status().is_success() && r.text().is_ok_and(|t| t == "pong"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both e2e tests grant egress through the process-wide `COMP_DEFAULT_EGRESS`
    /// and then start a Fleet, which reads it — so they must not interleave.
    static FLEET_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn form(name: &str) -> FormInput {
        FormInput {
            name: name.into(),
            description: "watches the build".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_plain_form_makes_a_valid_local_agent_with_memory_tools() {
        let s = spec_from_form(&form("Build-Watcher")).unwrap();
        assert_eq!(s.name, "build-watcher");
        assert_eq!(s.model, ModelSpec::Local);
        assert!(s.has_capability("remember") && s.has_capability("recall"));
        assert!(s.triggers.is_empty());
    }

    #[test]
    fn capabilities_parse_name_wit_and_description() {
        let mut f = form("a");
        f.capabilities = "http_get; fetch @ os:http/client.get | get a page; summarize | write 3 lines; recall | search carefully".into();
        let s = spec_from_form(&f).unwrap();
        let fetch = s.capabilities.iter().find(|c| c.name == "fetch").unwrap();
        assert_eq!(fetch.wit.as_deref(), Some("os:http/client.get"));
        assert_eq!(fetch.description, "get a page");
        assert!(s.has_capability("http_get") && s.has_capability("summarize"));
        let recall = s.capabilities.iter().filter(|c| c.name == "recall").collect::<Vec<_>>();
        assert_eq!(recall.len(), 1, "a built-in listed again overrides, never duplicates");
        assert_eq!(recall[0].description, "search carefully");
    }

    #[test]
    fn triggers_model_and_policy_fields() {
        let mut f = form("a");
        f.schedules = "*/10 * * * * :: check the feed; @hourly :: summarize".into();
        f.events = "deploy, alert".into();
        f.hosts = "example.com".into();
        f.auto_approve = "http_get".into();
        f.model = "anthropic:claude-haiku-4-5-20251001".into();
        let s = spec_from_form(&f).unwrap();
        assert_eq!(s.triggers.len(), 4);
        assert!(
            matches!(&s.triggers[0], Trigger::Schedule { cron, prompt } if cron == "*/10 * * * *" && prompt == "check the feed")
        );
        assert_eq!(s.allow_hosts, ["example.com"]);
        assert_eq!(s.auto_approve, ["http_get"]);
        assert!(matches!(s.model, ModelSpec::Anthropic { .. }));
    }

    #[test]
    fn bad_input_is_refused_with_a_reason() {
        assert!(spec_from_form(&form("agent-1")).is_err());
        let mut f = form("a");
        f.schedules = "every day".into();
        assert!(spec_from_form(&f).unwrap_err().contains("::"));
        f.schedules = "61 * * * * :: x".into();
        assert!(spec_from_form(&f).is_err());
        f.schedules.clear();
        f.model = "gpt".into();
        assert!(spec_from_form(&f).is_err());
        f.model.clear();
        f.description = "  ".into();
        assert!(spec_from_form(&f).is_err());
    }

    /// The whole claim, for real: the generic gateway deployed through
    /// platform-domain, reached over the real ingress, forwarding out of the
    /// tenant sandbox to the runtime (which only works if the operator's
    /// `default-egress` actually reached the manifest), and a cron trigger
    /// firing with nobody calling anything. Needs the built host/reconciler
    /// binaries and `cargo component`, like every Fleet test.
    #[test]
    fn a_gateway_agent_answers_over_the_lattice_and_a_schedule_fires_unprompted() {
        let _env = FLEET_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let tmp_guard =
            tempfile::Builder::new().prefix("gpui-console-e2e-").tempdir().expect("tempdir");
        let tmp = tmp_guard.path().to_path_buf();
        let _ = std::fs::remove_dir_all(&tmp);

        let mut cfg = Config::new(tmp.join("runtime"));
        cfg.approval_timeout = Duration::from_secs(5);
        let rt = Runtime::new(cfg).unwrap();
        let addr = server::serve(rt.clone(), "127.0.0.1:0", "t".into()).unwrap();
        rt.start_scheduler();
        std::env::set_var("COMP_DEFAULT_EGRESS", format!("127.0.0.1:{}", addr.port()));

        let host_args = vec![
            "--egress".to_string(),
            "127.0.0.1".to_string(),
            "--allow-private-egress".to_string(),
        ];
        let fleet = Fleet::start_with_platform_in_dir("e2e", 1, tmp.join("lattice"), &host_args);
        let base = fleet.platform_url();
        let token = register_and_login(&base, "e2e@agents.test", "password123");

        let mut spec = AgentSpec::new("echoer", "answers briefly");
        spec.model = ModelSpec::Mock { replies: vec!["pong from the runtime".to_string(); 20] };
        spec.triggers.push(Trigger::Schedule { cron: "@every 4s".into(), prompt: "tick".into() });
        rt.create_agent(spec).unwrap();

        upload_component(
            &base,
            &token,
            "echoer",
            gateway_wasm().unwrap(),
            &["runtime-url", "agent"],
        )
        .unwrap();
        let dep = create_deployment(
            &base,
            &token,
            "echoer",
            &format!("http://127.0.0.1:{}", addr.port()),
        )
        .unwrap();

        let host = "echoer.e2e.test";
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        while !ping_ready(fleet.ingress_port, host) {
            save_deployment(&base, &token, &dep);
            assert!(
                std::time::Instant::now() < deadline,
                "gateway never came up:\n{}",
                fleet.platform_log()
            );
            std::thread::sleep(Duration::from_secs(2));
        }
        // /ping cost nothing: no run yet.
        assert!(
            rt.store().runs("echoer", 5).is_empty()
                || rt.store().runs("echoer", 5).iter().all(|r| r.trigger != "http")
        );

        let a = ping(fleet.ingress_port, host, Some("hello there"), Duration::from_secs(30));
        let answer = a.expect("the gateway should reach the runtime and relay its answer");
        assert_eq!(answer, "pong from the runtime");
        let http_runs: Vec<_> =
            rt.store().runs("echoer", 20).into_iter().filter(|r| r.trigger == "http").collect();
        assert_eq!(http_runs.len(), 1);
        assert_eq!(
            http_runs[0].input, "hello there",
            "the question must arrive percent-decoded and intact"
        );

        // Schedule: fired without any request.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while !rt.store().runs("echoer", 20).iter().any(|r| r.trigger.starts_with("schedule")) {
            assert!(std::time::Instant::now() < deadline, "the @every trigger never fired");
            std::thread::sleep(Duration::from_millis(500));
        }

        // Pausing closes the front door too.
        rt.set_paused("echoer", true).unwrap();
        assert!(ping(fleet.ingress_port, host, Some("x"), Duration::from_secs(10)).is_err());

        rt.shutdown();
        drop(fleet);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The same path with the REAL on-device model instead of a script: an
    /// agent told a fact over the lattice must choose the `remember` tool by
    /// itself, and a second question must be answered from that memory.
    /// Skipped where `fm` doesn't exist (anything but Apple silicon macOS).
    #[test]
    fn a_gateway_agent_with_the_real_model_remembers_across_requests() {
        if !fm::is_available() {
            eprintln!("skipping: fm not available on this machine");
            return;
        }
        let _env = FLEET_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let tmp_guard =
            tempfile::Builder::new().prefix("gpui-console-e2e-fm-").tempdir().expect("tempdir");
        let tmp = tmp_guard.path().to_path_buf();
        let _ = std::fs::remove_dir_all(&tmp);
        let fm_server = fm::FmServer::start(Duration::from_secs(30)).expect("fm serve");

        let mut cfg = Config::new(tmp.join("runtime"));
        cfg.local =
            LocalModel { base_url: Some(fm_server.base_url().to_string()), model: "system".into() };
        let rt = Runtime::new(cfg).unwrap();
        let addr = server::serve(rt.clone(), "127.0.0.1:0", "t".into()).unwrap();
        std::env::set_var("COMP_DEFAULT_EGRESS", format!("127.0.0.1:{}", addr.port()));
        let host_args = vec![
            "--egress".to_string(),
            "127.0.0.1".to_string(),
            "--allow-private-egress".to_string(),
        ];
        let fleet = Fleet::start_with_platform_in_dir("e2efm", 1, tmp.join("lattice"), &host_args);
        let base = fleet.platform_url();
        let token = register_and_login(&base, "e2efm@agents.test", "password123");

        rt.create_agent(AgentSpec::new(
            "scribe",
            "You are a note keeper. When the user tells you a fact, save it with the remember tool. When asked about something, use recall first.",
        ))
        .unwrap();
        upload_component(
            &base,
            &token,
            "scribe",
            gateway_wasm().unwrap(),
            &["runtime-url", "agent"],
        )
        .unwrap();
        let dep = create_deployment(
            &base,
            &token,
            "scribe",
            &format!("http://127.0.0.1:{}", addr.port()),
        )
        .unwrap();
        let host = "scribe.e2efm.test";
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        while !ping_ready(fleet.ingress_port, host) {
            save_deployment(&base, &token, &dep);
            assert!(std::time::Instant::now() < deadline, "gateway never came up");
            std::thread::sleep(Duration::from_secs(2));
        }

        let t = Duration::from_secs(120);
        ping(fleet.ingress_port, host, Some("My dog is called Biscuit. Please remember that."), t)
            .unwrap();
        let saved = rt.store().memories("scribe");
        assert!(!saved.is_empty(), "the model never chose the remember tool");
        assert!(saved.iter().any(|m| m.text.to_lowercase().contains("biscuit")), "{saved:?}");

        let answer = ping(fleet.ingress_port, host, Some("What is my dog's name?"), t).unwrap();
        assert!(answer.to_lowercase().contains("biscuit"), "answer ignored the memory: {answer:?}");

        rt.shutdown();
        drop(fleet);
        drop(fm_server);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Runs WHATEVER agents a config directory describes, in parallel, over a
    /// real lattice — the agents are configuration, not part of this repo.
    /// Set `HOLON_AGENT_CONFIG_DIR` to a directory holding `agents/*.json`
    /// (the same specs the runtime reads) and an `e2e.json`:
    ///
    /// ```json
    /// { "window_secs": 110,
    ///   "min_runs": { "heartbeat": 4 },
    ///   "asks": { "rower": { "q": "a question", "any_of": ["expected", "substrings"] } } }
    /// ```
    ///
    /// Every agent gets a lattice front door. Each `ask` is sent over the real
    /// ingress while the scheduled agents keep firing; afterwards every agent
    /// in `min_runs` must have that many successful runs, every ask must have
    /// been answered with one of its `any_of` substrings, and some two agents'
    /// runs must have overlapped in time (they really ran in parallel).
    /// Skipped when the variable is unset or `fm` isn't available.
    #[test]
    fn configured_agents_run_in_parallel_over_the_lattice() {
        let Some(dir) = std::env::var_os("HOLON_AGENT_CONFIG_DIR").map(PathBuf::from) else {
            eprintln!("skipping: HOLON_AGENT_CONFIG_DIR is not set");
            return;
        };
        if !fm::is_available() {
            eprintln!("skipping: fm not available on this machine");
            return;
        }
        let _env = FLEET_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let plan: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("e2e.json")).expect("reading e2e.json"),
        )
        .expect("e2e.json is not valid JSON");
        let window = Duration::from_secs(plan["window_secs"].as_u64().unwrap_or(60));

        let tmp_guard =
            tempfile::Builder::new().prefix("gpui-console-e2e-cfg-").tempdir().expect("tempdir");
        let tmp = tmp_guard.path().to_path_buf();
        let _ = std::fs::remove_dir_all(&tmp);
        let fm_server = fm::FmServer::start(Duration::from_secs(30)).expect("fm serve");
        let mut cfg = Config::new(tmp.join("runtime"));
        cfg.local =
            LocalModel { base_url: Some(fm_server.base_url().to_string()), model: "system".into() };
        let rt = Runtime::new(cfg).unwrap();

        let mut names = Vec::new();
        for entry in std::fs::read_dir(dir.join("agents")).expect("agents/ directory").flatten() {
            if entry.path().extension().is_some_and(|e| e == "json") {
                let spec: AgentSpec =
                    serde_json::from_str(&std::fs::read_to_string(entry.path()).unwrap())
                        .unwrap_or_else(|e| panic!("{}: {e}", entry.path().display()));
                names.push(spec.name.clone());
                rt.create_agent(spec).unwrap_or_else(|e| panic!("{}: {e}", entry.path().display()));
            }
        }
        assert!(!names.is_empty(), "no agents in {}", dir.join("agents").display());

        let addr = server::serve(rt.clone(), "127.0.0.1:0", "t".into()).unwrap();
        std::env::set_var("COMP_DEFAULT_EGRESS", format!("127.0.0.1:{}", addr.port()));
        let host_args = vec![
            "--egress".to_string(),
            "127.0.0.1".to_string(),
            "--allow-private-egress".to_string(),
        ];
        let fleet = Fleet::start_with_platform_in_dir("cfg", 1, tmp.join("lattice"), &host_args);
        let base = fleet.platform_url();
        let token = register_and_login(&base, "cfg@agents.test", "password123");
        let wasm = gateway_wasm().unwrap();
        let runtime_url = format!("http://127.0.0.1:{}", addr.port());
        rt.start_scheduler();
        let started = std::time::Instant::now();

        let mut deps = Vec::new();
        for n in &names {
            upload_component(&base, &token, n, wasm.clone(), &["runtime-url", "agent"]).unwrap();
            deps.push((n.clone(), create_deployment(&base, &token, n, &runtime_url).unwrap()));
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        for (n, dep) in &deps {
            let host = format!("{n}.cfg.test");
            while !ping_ready(fleet.ingress_port, &host) {
                save_deployment(&base, &token, dep);
                assert!(std::time::Instant::now() < deadline, "{n}'s gateway never came up");
                std::thread::sleep(Duration::from_secs(2));
            }
        }

        // Every ask, concurrently, over the real ingress, while schedules fire.
        let ingress = fleet.ingress_port;
        let askers: Vec<_> = plan["asks"]
            .as_object()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|(agent, ask)| {
                let q = ask["q"].as_str().unwrap_or_default().to_string();
                std::thread::spawn(move || {
                    let r = ping(
                        ingress,
                        &format!("{agent}.cfg.test"),
                        Some(&q),
                        Duration::from_secs(300),
                    );
                    (agent, q, r, ask["any_of"].clone())
                })
            })
            .collect();
        let mut problems = Vec::new();
        for a in askers {
            let (agent, q, r, any_of) = a.join().unwrap();
            match r {
                Ok(answer) => {
                    println!("ASK {agent}: {q}\n  -> {answer}");
                    let want: Vec<&str> = any_of
                        .as_array()
                        .map(|v| v.iter().filter_map(Value::as_str).collect())
                        .unwrap_or_default();
                    if !want.is_empty() && !want.iter().any(|w| answer.contains(w)) {
                        problems.push(format!("{agent}: answer {answer:?} has none of {want:?}"));
                    }
                }
                Err(e) => problems.push(format!("{agent}: ask failed: {e}")),
            }
        }

        // Let the scheduled agents work out the rest of the window.
        if let Some(left) = window.checked_sub(started.elapsed()) {
            std::thread::sleep(left);
        }
        rt.shutdown();

        let mut spans: Vec<(String, u64, u64)> = Vec::new();
        for n in &names {
            let runs = rt.store().runs(n, 1000);
            let ok = runs.iter().filter(|r| r.status == Status::Ok).count();
            println!("AGENT {n}: {} run(s), {ok} ok", runs.len());
            for r in runs.iter().rev() {
                println!(
                    "  [{}] {} -> {}",
                    r.trigger,
                    short(&r.input, 40),
                    short(&r.answer.replace('\n', " "), 100)
                );
                spans.push((n.clone(), r.started, r.finished));
            }
            let need = plan["min_runs"][n.as_str()].as_u64().unwrap_or(0) as usize;
            if ok < need {
                problems.push(format!("{n}: {ok} successful run(s), wanted at least {need}"));
            }
        }
        let overlapped = spans.iter().any(|(a, s1, e1)| {
            spans.iter().any(|(b, s2, e2)| {
                a != b && s1 <= e2 && s2 <= e1 && (e1 > s1 || e2 > s2 || s1 == s2)
            })
        });
        if names.len() > 1 && !overlapped {
            problems.push("no two agents' runs overlapped — they did not run in parallel".into());
        }

        drop(fleet);
        drop(fm_server);
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(problems.is_empty(), "e2e problems:\n{}", problems.join("\n"));
    }
}
