//! The runtime: owns the store, bus and shared KV, runs agents, fires their
//! triggers, and holds approvals open while a human decides. One per state
//! directory.
//!
//! ## How agents wake each other
//!
//! * **Call** (`agent:<name>`) — synchronous; the caller gets the answer and
//!   the tokens it cost, charged to its own budget.
//! * **Task** (`spawn_task` / `task_result`) — the same, without waiting.
//! * **Event** (`emit_event`) — durable, fan-out. Written to the bus log
//!   first, then delivered to each subscriber from its own offset, so a paused
//!   agent or a restart loses nothing.
//! * **Store** (`store_put`) — a write that CHANGES a shared value publishes
//!   `store.<ns>`; agents with a `StoreChange` trigger wake on it.
//! * **Timer** (`schedule_self`) — a persisted one-shot wake-up.
//!
//! Every hop carries a [`Cause`](crate::agent::Cause): the W3C trace id, the
//! parent span, hops, the `agent|topic` chain and the remaining token budget.
//! That is what the guards below, and the trace export, are built on.
//!
//! ## What stops a runaway
//!
//! A chain longer than `MAX_HOPS`, an agent woken twice by the same topic in
//! one chain (a cycle), more than `max_runs_per_min` runs, and an agent whose
//! last few runs all failed (circuit breaker) are all REFUSED before the model
//! is called, and logged as `Dropped` runs so the refusal is visible.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::agent::{self, Cause, Event, Host, TaskState, MAX_HOPS};
use crate::bus::{Bus, Draft, Envelope, FileBus};
use crate::cron;
use crate::kv::{Entry, Kv, Put};
use crate::model::{self, LocalModel, Msg, Reply};
use crate::spec::{validate_name, validate_topic, AgentSpec, Trigger};
use crate::store::{RunRecord, Status, Store};
use crate::trace::{new_span_id, new_trace_id};

#[derive(Clone)]
pub struct Config {
    pub state_dir: std::path::PathBuf,
    /// Where `ModelSpec::Local` points. Settable later (`set_local_model`)
    /// because a console starts the runtime before its model server is up.
    pub local: LocalModel,
    /// How long a sensitive tool call waits for a human before it is denied.
    pub approval_timeout: Duration,
    /// Consecutive failed runs after which an agent's circuit opens.
    pub breaker_threshold: u32,
    /// How long an open circuit refuses runs.
    pub breaker_cooldown: Duration,
    /// An OpenTelemetry collector's OTLP/HTTP base URL
    /// (`http://localhost:4318`). Every finished run is exported to
    /// `<endpoint>/v1/traces`. The standard `OTEL_EXPORTER_OTLP_ENDPOINT`
    /// variable is honoured by the daemon and the console.
    pub otlp_endpoint: Option<String>,
    /// On-device speech engines (see `speech.rs`). Nothing is configured by default.
    pub speech: crate::speech::SpeechConfig,
    /// Where the embedding service listens (`embed/server.py`); empty: memories are recalled by shared words.
    pub embed_url: String,
    /// The launchd service behind `embed_url`, started on demand (see `lazy.rs`).
    pub embed_service: String,
}

impl Config {
    pub fn new(state_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            local: LocalModel::default(),
            approval_timeout: Duration::from_secs(300),
            breaker_threshold: 5,
            breaker_cooldown: Duration::from_secs(120),
            otlp_endpoint: None,
            speech: Default::default(),
            embed_url: String::new(),
            embed_service: String::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Pending {
    pub id: u64,
    pub agent: String,
    pub tool: String,
    pub args: Value,
    /// What led to this request (`agent|topic` per wake-up), oldest first.
    pub chain: Vec<String>,
}

struct Slot {
    pending: Pending,
    answer: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Timer {
    id: u64,
    agent: String,
    at: u64,
    prompt: String,
}

const MAX_TIMERS_PER_AGENT: usize = 20;
const MAX_TASKS_KEPT: usize = 200;

pub struct Runtime {
    /// Weak self, so `Host` methods (which only have `&self`) can hand an owned
    /// handle to the threads they spawn.
    me: std::sync::Weak<Runtime>,
    store: Store,
    kv: Kv,
    bus: Box<dyn Bus>,
    cfg: Config,
    speech: crate::speech::Speech,
    embedder: Option<crate::embed::Embedder>,
    local: Mutex<LocalModel>,
    started: u64,
    busy: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    approvals: Mutex<Vec<Slot>>,
    approvals_cv: Condvar,
    next_approval: AtomicU64,
    observers: Mutex<Vec<Sender<Event>>>,
    mock_cursor: Mutex<HashMap<String, usize>>,
    last_fired: Mutex<HashMap<(String, usize), u64>>,
    stop: AtomicBool,
    tasks: Mutex<Vec<(String, TaskState)>>,
    next_task: AtomicU64,
    timers: Mutex<Vec<Timer>>,
    next_timer: AtomicU64,
    /// Agents with a bus drain in flight; a second kick is folded into it.
    draining: Mutex<HashSet<String>>,
    rate: Mutex<HashMap<String, VecDeque<u64>>>,
    /// agent -> (consecutive failures, circuit open until)
    breaker: Mutex<HashMap<String, (u32, u64)>>,
}

pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn unix_now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Checked before a spec is saved, so a typo is an error now rather than an
/// agent that silently never fires or never gets to speak.
pub fn validate_spec(spec: &AgentSpec) -> Result<(), String> {
    validate_name(&spec.name)?;
    if spec.description.trim().is_empty() {
        return Err("description is empty — it is the agent's purpose and its system prompt".into());
    }
    if !(1..=50).contains(&spec.max_steps) {
        return Err("max_steps must be 1-50".into());
    }
    let mut seen = HashSet::new();
    for c in &spec.capabilities {
        if c.name.is_empty() || !seen.insert(c.name.clone()) {
            return Err(format!("capability `{}` is empty or listed twice", c.name));
        }
        if let Some(t) = c.name.strip_prefix("agent:") {
            validate_name(t)?;
        }
    }
    for t in &spec.must_call {
        if !spec.has_capability(t) {
            return Err(format!("must_call `{t}` is not one of this agent's capabilities"));
        }
    }
    for t in &spec.topics_out {
        validate_topic(&t.replace('*', "x")).map_err(|e| format!("topics_out: {e}"))?;
    }
    for ns in spec.store.read.iter().chain(&spec.store.write) {
        validate_topic(ns).map_err(|e| format!("store: {e}"))?;
    }
    for t in &spec.triggers {
        match t {
            Trigger::Schedule { cron, prompt } => {
                cron::parse(cron)?;
                if prompt.trim().is_empty() {
                    return Err(
                        "a schedule trigger needs a prompt — the task it gives the agent".into()
                    );
                }
            }
            Trigger::Event { topic, .. } => {
                validate_topic(topic).map_err(|e| format!("event trigger: {e}"))?
            }
            Trigger::StoreChange { ns, .. } => {
                validate_topic(ns).map_err(|e| format!("store trigger: {e}"))?
            }
        }
    }
    Ok(())
}

/// How long an agent calling another agent waits for it to finish its current run.
const CALL_WAIT: std::time::Duration = std::time::Duration::from_secs(90);

/// Whether a finished run is the kind of failure another model might not have.
fn needs_better_model(rec: &RunRecord) -> bool {
    (rec.status == Status::Failed && rec.answer.starts_with("never called required tool"))
        || (rec.status == Status::Ok && rec.answer.trim().is_empty())
}

thread_local! {
    /// The agents whose runs are in progress on this thread, outermost first: a call from
    /// an agent's run is made on its thread, so this is the call stack of agents.
    static RUNNING_HERE: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl Runtime {
    pub fn new(cfg: Config) -> Result<Arc<Self>, String> {
        let store = Store::open(&cfg.state_dir)?;
        let kv = Kv::open(&cfg.state_dir)?;
        let bus = FileBus::open(&cfg.state_dir)?;
        let timers: Vec<Timer> = std::fs::read_to_string(cfg.state_dir.join("timers.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let next_timer = timers.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        let rt = Arc::new_cyclic(|me| Self {
            me: me.clone(),
            store,
            kv,
            bus: Box::new(bus),
            local: Mutex::new(cfg.local.clone()),
            speech: crate::speech::Speech::new(cfg.speech.clone()),
            embedder: crate::embed::Embedder::new(&cfg.embed_url, &cfg.embed_service),
            cfg,
            started: unix_now(),
            busy: Mutex::new(HashMap::new()),
            approvals: Mutex::new(Vec::new()),
            approvals_cv: Condvar::new(),
            next_approval: AtomicU64::new(1),
            observers: Mutex::new(Vec::new()),
            mock_cursor: Mutex::new(HashMap::new()),
            last_fired: Mutex::new(HashMap::new()),
            stop: AtomicBool::new(false),
            tasks: Mutex::new(Vec::new()),
            next_task: AtomicU64::new(1),
            timers: Mutex::new(timers),
            next_timer: AtomicU64::new(next_timer),
            draining: Mutex::new(HashSet::new()),
            rate: Mutex::new(HashMap::new()),
            breaker: Mutex::new(HashMap::new()),
        });
        for spec in rt.store.list() {
            rt.ensure_offsets(&spec);
        }
        Ok(rt)
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn kv(&self) -> &Kv {
        &self.kv
    }

    /// Embeds `texts` with the embedding service (starting it if it is on demand). `None` when
    /// none is configured or it cannot answer.
    pub fn has_embedder(&self) -> bool {
        self.embedder.is_some()
    }

    pub fn embed_texts(&self, texts: &[String], kind: &str) -> Option<Vec<Vec<f32>>> {
        self.embedder.as_ref()?.embed(texts, kind)
    }

    pub fn speech(&self) -> &crate::speech::Speech {
        &self.speech
    }

    pub fn bus(&self) -> &dyn Bus {
        self.bus.as_ref()
    }

    pub fn set_local_model(&self, m: LocalModel) {
        *self.local.lock().unwrap() = m;
    }

    pub fn subscribe(&self) -> Receiver<Event> {
        let (tx, rx) = channel();
        self.observers.lock().unwrap().push(tx);
        rx
    }

    /// One trace as an OTLP/JSON `ExportTraceServiceRequest`: the same
    /// document a collector would have received, so any OpenTelemetry tool can
    /// open it.
    pub fn trace_otlp(&self, trace_id: &str) -> Value {
        crate::otlp::request(&self.store.trace(trace_id))
    }

    // ---- lifecycle --------------------------------------------------------

    /// A new subscriber starts at the head of each topic it listens on: it
    /// hears what happens from now on, not the whole history of the topic.
    fn ensure_offsets(&self, spec: &AgentSpec) {
        for topic in spec.triggers.iter().filter_map(Trigger::topic) {
            if let (Ok(None), Ok(head)) =
                (self.bus.offset(&topic, &spec.name), self.bus.head(&topic))
            {
                let _ = self.bus.set_offset(&topic, &spec.name, head);
            }
        }
    }

    pub fn create_agent(&self, spec: AgentSpec) -> Result<(), String> {
        validate_spec(&spec)?;
        if self.store.get(&spec.name).is_some() {
            return Err(format!("{} already exists", spec.name));
        }
        self.store.put(&spec)?;
        self.ensure_offsets(&spec);
        Ok(())
    }

    /// Replaces the spec. The next run uses it — no rebuild, no redeploy.
    pub fn update_agent(&self, spec: AgentSpec) -> Result<(), String> {
        validate_spec(&spec)?;
        if self.store.get(&spec.name).is_none() {
            return Err(format!("no agent named {}", spec.name));
        }
        self.store.put(&spec)?;
        self.ensure_offsets(&spec);
        Ok(())
    }

    /// Resuming delivers everything the agent missed while paused.
    pub fn set_paused(self: &Arc<Self>, name: &str, paused: bool) -> Result<(), String> {
        let mut s = self.store.get(name).ok_or_else(|| format!("no agent named {name}"))?;
        s.paused = paused;
        self.store.put(&s)?;
        if !paused {
            self.kick(name);
        }
        Ok(())
    }

    /// Creates or replaces a project. Every member must be an existing agent.
    pub fn put_project(&self, project: crate::projects::Project) -> Result<(), String> {
        project.validate()?;
        if let Some(missing) = project.agents.iter().find(|a| self.store.get(a).is_none()) {
            return Err(format!("no agent named {missing}"));
        }
        self.store.put_project(&project)
    }

    pub fn delete_project(&self, name: &str) -> Result<(), String> {
        self.store.delete_project(name)
    }

    pub fn add_to_project(&self, name: &str, agent: &str) -> Result<(), String> {
        let mut p =
            self.store.get_project(name).ok_or_else(|| format!("no project named {name}"))?;
        if !p.agents.iter().any(|a| a == agent) {
            p.agents.push(agent.to_string());
        }
        self.put_project(p)
    }

    /// Takes the agent's project grants away at its next run. If it was the
    /// project's lead, the project has no lead until one is set.
    pub fn remove_from_project(&self, name: &str, agent: &str) -> Result<(), String> {
        let mut p =
            self.store.get_project(name).ok_or_else(|| format!("no project named {name}"))?;
        p.agents.retain(|a| a != agent);
        if p.lead.as_deref() == Some(agent) {
            p.lead = None;
        }
        self.put_project(p)
    }

    pub fn delete_agent(&self, name: &str) -> Result<(), String> {
        self.store.delete(name)?;
        for p in
            self.store.list_projects().into_iter().filter(|p| p.agents.iter().any(|a| a == name))
        {
            let _ = self.remove_from_project(&p.name, name);
        }
        self.last_fired.lock().unwrap().retain(|(n, _), _| n != name);
        self.timers.lock().unwrap().retain(|t| t.agent != name);
        self.save_timers();
        Ok(())
    }

    // ---- running ----------------------------------------------------------

    fn lock_for(&self, name: &str) -> Arc<Mutex<()>> {
        self.busy.lock().unwrap().entry(name.to_string()).or_default().clone()
    }

    /// Refuses a run before the model is called: too many hops, a cycle, the
    /// rate limit, an open circuit. `Err` is the reason.
    fn admit(&self, spec: &AgentSpec, cause: &Cause, via: &str) -> Result<(), String> {
        let now = unix_now();
        if cause.hops > MAX_HOPS {
            return Err(format!("chain is {} wake-ups deep; the limit is {MAX_HOPS}", cause.hops));
        }
        let key = format!("{}|{via}", spec.name);
        if !via.is_empty() && cause.chain.contains(&key) {
            return Err(format!(
                "cycle: {} was already woken via `{via}` earlier in this chain",
                spec.name
            ));
        }
        if let Some(&(_, until)) = self.breaker.lock().unwrap().get(&spec.name) {
            if until > now {
                return Err(format!("circuit open for {}s after repeated failures", until - now));
            }
        }
        if spec.max_runs_per_min > 0 {
            let mut rate = self.rate.lock().unwrap();
            let q = rate.entry(spec.name.clone()).or_default();
            while q.front().is_some_and(|t| now.saturating_sub(*t) >= 60) {
                q.pop_front();
            }
            if q.len() >= spec.max_runs_per_min as usize {
                return Err(format!("rate limit: {} runs per minute", spec.max_runs_per_min));
            }
            q.push_back(now);
        }
        Ok(())
    }

    fn note_outcome(&self, agent: &str, ok: bool) {
        let mut b = self.breaker.lock().unwrap();
        let e = b.entry(agent.to_string()).or_insert((0, 0));
        if ok {
            *e = (0, 0);
        } else {
            e.0 += 1;
            if e.0 >= self.cfg.breaker_threshold {
                e.1 = unix_now() + self.cfg.breaker_cooldown.as_secs();
                e.0 = 0;
            }
        }
    }

    /// A refused wake-up still leaves a record (and a span): silence would
    /// look like the agent ignoring it.
    fn record_drop(&self, agent: &str, trigger: &str, input: &str, cause: &Cause, why: &str) {
        let (now, now_ms) = (unix_now(), unix_now_ms());
        let seq = self.next_task.fetch_add(1, Ordering::Relaxed);
        let rec = RunRecord {
            id: format!("{agent}-{now}-drop{seq}"),
            agent: agent.to_string(),
            trigger: trigger.to_string(),
            input: input.to_string(),
            started: now,
            finished: now,
            status: Status::Dropped,
            answer: format!("dropped: {why}"),
            steps: Vec::new(),
            tokens_in: 0,
            tokens_out: 0,
            started_ms: now_ms,
            finished_ms: now_ms,
            model: String::new(),
            trace_id: if cause.trace_id.is_empty() {
                new_trace_id()
            } else {
                cause.trace_id.clone()
            },
            span_id: new_span_id(),
            parent_span_id: cause.parent_span_id.clone(),
            hops: cause.hops,
            chain: cause.chain.clone(),
        };
        let _ = self.store.record_run(&rec);
        self.observe(Event::RunFinished(Box::new(rec)));
    }

    /// Runs `name` to completion. One run per agent at a time: with `wait`
    /// it queues behind the current one, without it a busy agent is an error
    /// (a schedule tick that finds its agent still working should skip, not
    /// pile up behind it). `via` names how it was woken (`topic` or `call`)
    /// for cycle detection; empty for triggers that cannot cycle.
    pub fn run_agent(
        &self,
        name: &str,
        trigger: &str,
        input: &str,
        cause: &Cause,
        via: &str,
        wait: bool,
    ) -> Result<RunRecord, String> {
        let spec = self.store.get(name).ok_or_else(|| format!("no agent named {name}"))?;
        if spec.paused {
            return Err(format!("{name} is paused"));
        }
        // What a run may do is its own spec plus whatever its projects grant.
        let directory: Vec<(String, String)> =
            self.store.list().into_iter().map(|a| (a.name, a.description)).collect();
        let mut spec = crate::projects::effective(&spec, &self.store.list_projects(), &directory);
        // A capability named after a shared connector gets its definition from there.
        let dir = self.store.connectors_dir();
        for c in spec.capabilities.iter_mut().filter(|c| c.exec.is_none()) {
            if let Some(d) = crate::connector::load(&dir, &c.name) {
                c.exec = Some(d.exec);
                if c.description.is_empty() {
                    c.description = d.description;
                }
            }
        }
        let lock = self.lock_for(name);
        let _guard = if wait {
            lock.lock().unwrap()
        } else if via == "call" {
            // An agent further up this very call stack is waiting on us: queueing
            // behind it would be a certain deadlock.
            if RUNNING_HERE.with(|r| r.borrow().iter().any(|n| n == name)) {
                return Err(format!("{name} is already waiting on this call"));
            }
            // An agent asked by another agent queues behind the run in progress (the
            // owner may have just addressed both). Bounded, so two agents waiting on
            // each other time out with an error instead of hanging.
            let deadline = std::time::Instant::now() + CALL_WAIT;
            loop {
                match lock.try_lock() {
                    Ok(g) => break g,
                    Err(_) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(100))
                    }
                    Err(_) => {
                        return Err(format!("{name} stayed busy for {}s", CALL_WAIT.as_secs()))
                    }
                }
            }
        } else {
            lock.try_lock().map_err(|_| format!("{name} is busy with another run"))?
        };
        if let Err(why) = self.admit(&spec, cause, via) {
            self.record_drop(name, trigger, input, cause, &why);
            return Err(format!("dropped: {why}"));
        }
        // The chain a run records includes its own wake-up.
        let mut c = cause.clone();
        if !via.is_empty() {
            c.chain.push(format!("{name}|{via}"));
        }
        RUNNING_HERE.with(|r| r.borrow_mut().push(name.to_string()));
        let mut rec = agent::run(self, &spec, trigger, input, &c);
        // A run that fails a check the agent sets (a tool it must call) or says nothing is
        // tried again on the next model of its fallback chain, so a hosted model's miss is
        // the local model's turn, and a good hosted answer never wakes a local server.
        if let crate::spec::ModelSpec::Fallback { models } = &spec.model {
            let mut rest = &models[..];
            while rest.len() > 1 && needs_better_model(&rec) {
                rest = &rest[1..];
                let mut again = spec.clone();
                again.model = if rest.len() == 1 {
                    rest[0].clone()
                } else {
                    crate::spec::ModelSpec::Fallback { models: rest.to_vec() }
                };
                rec = agent::run(self, &again, trigger, input, &c);
            }
        }
        RUNNING_HERE.with(|r| r.borrow_mut().pop());
        self.note_outcome(name, rec.status == Status::Ok);
        Ok(rec)
    }

    // ---- bus --------------------------------------------------------------

    fn publish_draft(self: &Arc<Self>, topic: &str, draft: Draft) -> Result<usize, String> {
        validate_topic(topic)?;
        if draft.hops > MAX_HOPS {
            return Err(format!("chain is {} wake-ups deep; the limit is {MAX_HOPS}", draft.hops));
        }
        let env = self.bus.publish(topic, draft)?;
        let mut woken = 0;
        for spec in self.store.list() {
            let subscribed = spec
                .triggers
                .iter()
                .any(|t| t.topic().as_deref() == Some(topic) && t.matches(&env.draft.payload));
            if subscribed {
                if !spec.paused {
                    woken += 1;
                }
                // A paused agent is kicked too: the drain refuses while it is
                // paused and `set_paused(false)` kicks again, so this keeps
                // one code path.
                self.kick(&spec.name);
            }
        }
        Ok(woken)
    }

    /// Publishes from outside any agent (a webhook, the admin API), joining
    /// `trace_id` / `parent_span_id` if the caller supplied a `traceparent`.
    /// Returns how many agents it will wake.
    pub fn emit_event(
        self: &Arc<Self>,
        topic: &str,
        payload: &str,
        trace_id: Option<&str>,
        parent_span_id: Option<&str>,
    ) -> Result<usize, String> {
        self.publish_draft(
            topic,
            Draft {
                from: "external".into(),
                payload: payload.to_string(),
                trace_id: trace_id.map(String::from).unwrap_or_else(new_trace_id),
                parent_span_id: parent_span_id.map(String::from),
                hops: 1,
                ts: unix_now(),
                ..Default::default()
            },
        )
    }

    /// Starts draining `agent`'s backlog on its own thread, unless one is
    /// already running (it will see anything newly published).
    pub fn kick(self: &Arc<Self>, agent: &str) {
        if !self.draining.lock().unwrap().insert(agent.to_string()) {
            return;
        }
        let (rt, agent) = (self.clone(), agent.to_string());
        std::thread::spawn(move || loop {
            while let Some((topic, env)) = rt.next_pending(&agent) {
                rt.deliver(&agent, &topic, &env);
                let _ = rt.bus.set_offset(&topic, &agent, env.seq);
            }
            rt.draining.lock().unwrap().remove(&agent);
            // Something may have been published between the last empty check
            // and the flag clearing; look once more.
            if rt.next_pending(&agent).is_none()
                || !rt.draining.lock().unwrap().insert(agent.clone())
            {
                break;
            }
        });
    }

    /// The oldest undelivered envelope across the agent's topics. `None` when
    /// there is none, or the agent is paused or gone.
    fn next_pending(&self, agent: &str) -> Option<(String, Envelope)> {
        let spec = self.store.get(agent)?;
        if spec.paused {
            return None;
        }
        let topics: HashSet<String> = spec.triggers.iter().filter_map(Trigger::topic).collect();
        let mut best: Option<(String, Envelope)> = None;
        for topic in topics {
            let offset = match self.bus.offset(&topic, agent) {
                Ok(Some(o)) => o,
                Ok(None) => self.bus.head(&topic).ok()?,
                Err(_) => continue,
            };
            if let Ok(mut evs) = self.bus.read_after(&topic, offset, 1) {
                if let Some(e) = evs.pop() {
                    let older = best
                        .as_ref()
                        .is_none_or(|(_, b)| (e.draft.ts, e.seq) < (b.draft.ts, b.seq));
                    if older {
                        best = Some((topic, e));
                    }
                }
            }
        }
        best
    }

    fn deliver(&self, agent: &str, topic: &str, env: &Envelope) {
        let Some(spec) = self.store.get(agent) else { return };
        let matched = spec
            .triggers
            .iter()
            .find(|t| t.topic().as_deref() == Some(topic) && t.matches(&env.draft.payload));
        let Some(trigger) = matched else { return };
        let label = match trigger {
            Trigger::StoreChange { .. } => {
                let key = serde_json::from_str::<Value>(&env.draft.payload)
                    .ok()
                    .and_then(|v| v["key"].as_str().map(String::from))
                    .unwrap_or_default();
                format!("store: {}/{key}", topic.trim_start_matches("store."))
            }
            _ => format!("event: {topic}"),
        };
        let cause = Cause {
            trace_id: env.draft.trace_id.clone(),
            parent_span_id: env.draft.parent_span_id.clone(),
            hops: env.draft.hops,
            chain: env.draft.chain.clone(),
            budget: env.draft.budget,
        };
        let _ = self.run_agent(agent, &label, &env.draft.payload, &cause, topic, true);
    }

    /// Delivers whatever agents missed while the runtime was down.
    pub fn recover(self: &Arc<Self>) {
        for spec in self.store.list() {
            if spec.triggers.iter().any(|t| t.topic().is_some()) {
                self.kick(&spec.name);
            }
        }
    }

    // ---- schedule and timers ---------------------------------------------

    fn save_timers(&self) {
        let t = self.timers.lock().unwrap();
        let _ = std::fs::write(
            self.cfg.state_dir.join("timers.json"),
            serde_json::to_string(&*t).unwrap_or_default(),
        );
    }

    /// Evaluates every schedule trigger and due timer once, firing what is
    /// due. Public so a test can drive it without waiting on the thread.
    pub fn tick(self: &Arc<Self>, now: u64) -> usize {
        let mut fired = 0;
        for spec in self.store.list() {
            if spec.paused {
                continue;
            }
            for (i, t) in spec.triggers.iter().enumerate() {
                let Trigger::Schedule { cron: expr, prompt } = t else { continue };
                let Ok(sched) = cron::parse(expr) else { continue };
                let key = (spec.name.clone(), i);
                let last = self.last_fired.lock().unwrap().get(&key).copied();
                if sched.due(now, last, self.started) {
                    self.last_fired.lock().unwrap().insert(key, now);
                    fired += 1;
                    let (rt, name, trig, input) = (
                        self.clone(),
                        spec.name.clone(),
                        format!("schedule: {expr}"),
                        prompt.clone(),
                    );
                    std::thread::spawn(move || {
                        let _ = rt.run_agent(&name, &trig, &input, &Cause::default(), "", false);
                    });
                }
            }
        }
        let due: Vec<Timer> = {
            let mut timers = self.timers.lock().unwrap();
            let (due, rest): (Vec<_>, Vec<_>) = timers.drain(..).partition(|t| t.at <= now);
            *timers = rest;
            due
        };
        if !due.is_empty() {
            self.save_timers();
        }
        for t in due {
            fired += 1;
            let rt = self.clone();
            std::thread::spawn(move || {
                let _ = rt.run_agent(&t.agent, "timer", &t.prompt, &Cause::default(), "", true);
            });
        }
        fired
    }

    /// Starts the background scheduler (once a second) and delivers any bus
    /// backlog left from before this process started.
    pub fn start_scheduler(self: &Arc<Self>) {
        self.recover();
        let rt = self.clone();
        std::thread::spawn(move || {
            while !rt.stop.load(Ordering::Relaxed) {
                rt.tick(unix_now());
                std::thread::sleep(Duration::from_secs(1));
            }
        });
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    // ---- shared store -----------------------------------------------------

    /// Writes from outside any agent (the admin API). Wakes watchers on change.
    pub fn put_store(
        self: &Arc<Self>,
        ns: &str,
        key: &str,
        value: &str,
        by: &str,
    ) -> Result<Put, String> {
        self.kv_put_with(by, ns, key, value, None, &Cause::default())
    }

    fn kv_put_with(
        self: &Arc<Self>,
        by: &str,
        ns: &str,
        key: &str,
        value: &str,
        if_version: Option<u64>,
        cause: &Cause,
    ) -> Result<Put, String> {
        let put = self.kv.put(ns, key, value, by, unix_now(), if_version)?;
        if put.changed {
            let clipped: String = value.chars().take(4_000).collect();
            let payload =
                json!({"ns": ns, "key": key, "version": put.version, "value": clipped, "by": by});
            self.publish_draft(
                &format!("store.{ns}"),
                Draft {
                    from: by.to_string(),
                    payload: payload.to_string(),
                    trace_id: if cause.trace_id.is_empty() {
                        new_trace_id()
                    } else {
                        cause.trace_id.clone()
                    },
                    parent_span_id: cause.parent_span_id.clone(),
                    hops: cause.hops + 1,
                    chain: cause.chain.clone(),
                    budget: cause.budget,
                    ts: unix_now(),
                },
            )?;
        }
        Ok(put)
    }

    // ---- tasks ------------------------------------------------------------

    fn set_task(&self, id: &str, state: TaskState) {
        let mut tasks = self.tasks.lock().unwrap();
        match tasks.iter_mut().find(|(i, _)| i == id) {
            Some(slot) => slot.1 = state,
            None => {
                tasks.push((id.to_string(), state));
                if tasks.len() > MAX_TASKS_KEPT {
                    tasks.remove(0);
                }
            }
        }
    }

    // ---- approvals --------------------------------------------------------

    pub fn pending_approvals(&self) -> Vec<Pending> {
        self.approvals
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.answer.is_none())
            .map(|s| s.pending.clone())
            .collect()
    }

    /// `false` if no such pending request (already answered or timed out).
    pub fn resolve_approval(&self, id: u64, approved: bool) -> bool {
        let mut list = self.approvals.lock().unwrap();
        match list.iter_mut().find(|s| s.pending.id == id && s.answer.is_none()) {
            Some(s) => {
                s.answer = Some(approved);
                self.approvals_cv.notify_all();
                true
            }
            None => false,
        }
    }

    fn arc(&self) -> Result<Arc<Runtime>, String> {
        self.me.upgrade().ok_or_else(|| "runtime is shutting down".to_string())
    }
}

impl Host for Runtime {
    fn now(&self) -> u64 {
        unix_now()
    }

    fn now_ms(&self) -> u64 {
        unix_now_ms()
    }

    fn store(&self) -> &Store {
        &self.store
    }

    fn model(&self, spec: &AgentSpec, system: &str, msgs: &[Msg]) -> Result<Reply, String> {
        let local = self.local.lock().unwrap().clone();
        let cursor =
            Mutex::new(self.mock_cursor.lock().unwrap().get(&spec.name).copied().unwrap_or(0));
        let r = model::complete(&spec.model, &local, system, msgs, &cursor);
        self.mock_cursor.lock().unwrap().insert(spec.name.clone(), *cursor.lock().unwrap());
        r
    }

    fn approve(&self, agent: &str, tool: &str, args: &Value, chain: &[String]) -> bool {
        let id = self.next_approval.fetch_add(1, Ordering::Relaxed);
        let pending = Pending {
            id,
            agent: agent.to_string(),
            tool: tool.to_string(),
            args: args.clone(),
            chain: chain.to_vec(),
        };
        self.approvals.lock().unwrap().push(Slot { pending, answer: None });
        self.observe(Event::ApprovalRequested {
            id,
            agent: agent.to_string(),
            tool: tool.to_string(),
            args: args.clone(),
            chain: chain.to_vec(),
        });

        let deadline = std::time::Instant::now() + self.cfg.approval_timeout;
        let mut list = self.approvals.lock().unwrap();
        let answer = loop {
            if let Some(a) = list.iter().find(|s| s.pending.id == id).and_then(|s| s.answer) {
                break a;
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break false;
            }
            list = self.approvals_cv.wait_timeout(list, left).unwrap().0;
        };
        list.retain(|s| s.pending.id != id);
        drop(list);
        self.observe(Event::ApprovalResolved { id, approved: answer });
        answer
    }

    fn call_agent(
        &self,
        caller: &str,
        target: &str,
        message: &str,
        cause: &Cause,
    ) -> Result<(String, u64), String> {
        if target == caller {
            return Err("an agent cannot delegate to itself".into());
        }
        let child = Cause { hops: cause.hops + 1, ..cause.clone() };
        let r =
            self.run_agent(target, &format!("agent: {caller}"), message, &child, "call", false)?;
        match r.status {
            Status::Ok => Ok((r.answer, r.tokens_in + r.tokens_out)),
            _ => Err(format!("{target} failed: {}", r.answer)),
        }
    }

    fn spawn_task(
        &self,
        caller: &str,
        target: &str,
        message: &str,
        cause: &Cause,
    ) -> Result<String, String> {
        if target == caller {
            return Err("an agent cannot start itself as a task".into());
        }
        let spec = self.store.get(target).ok_or_else(|| format!("no agent named {target}"))?;
        if spec.paused {
            return Err(format!("{target} is paused"));
        }
        let rt = self.arc()?;
        let id = format!("task-{}", self.next_task.fetch_add(1, Ordering::Relaxed));
        self.set_task(&id, TaskState::Running);
        let child = Cause { hops: cause.hops + 1, ..cause.clone() };
        let (tid, target, caller, message) =
            (id.clone(), target.to_string(), caller.to_string(), message.to_string());
        std::thread::spawn(move || {
            let state = match rt.run_agent(
                &target,
                &format!("agent: {caller}"),
                &message,
                &child,
                "call",
                true,
            ) {
                Ok(r) => TaskState::Done { ok: r.status == Status::Ok, answer: r.answer },
                Err(e) => TaskState::Done { ok: false, answer: e },
            };
            rt.set_task(&tid, state);
        });
        Ok(id)
    }

    fn task_result(&self, id: &str) -> Option<TaskState> {
        self.tasks.lock().unwrap().iter().find(|(i, _)| i == id).map(|(_, s)| s.clone())
    }

    fn emit(&self, from: &str, topic: &str, payload: &str, cause: &Cause) -> Result<usize, String> {
        self.arc()?.publish_draft(
            topic,
            Draft {
                from: from.to_string(),
                payload: payload.to_string(),
                trace_id: if cause.trace_id.is_empty() {
                    new_trace_id()
                } else {
                    cause.trace_id.clone()
                },
                parent_span_id: cause.parent_span_id.clone(),
                hops: cause.hops + 1,
                chain: cause.chain.clone(),
                budget: cause.budget,
                ts: unix_now(),
            },
        )
    }

    fn kv_get(&self, ns: &str, key: &str) -> Result<Option<Entry>, String> {
        self.kv.get(ns, key)
    }

    fn kv_list(&self, ns: &str, prefix: &str) -> Result<Vec<(String, Entry)>, String> {
        self.kv.list(ns, prefix)
    }

    fn kv_put(
        &self,
        by: &str,
        ns: &str,
        key: &str,
        value: &str,
        if_version: Option<u64>,
        cause: &Cause,
    ) -> Result<Put, String> {
        self.arc()?.kv_put_with(by, ns, key, value, if_version, cause)
    }

    fn schedule_self(
        &self,
        agent: &str,
        in_secs: u64,
        prompt: &str,
        _cause: &Cause,
    ) -> Result<(), String> {
        if prompt.trim().is_empty() {
            return Err("`prompt` is empty".into());
        }
        if !(1..=30 * 86_400).contains(&in_secs) {
            return Err("in_secs must be between 1 second and 30 days".into());
        }
        {
            let mut timers = self.timers.lock().unwrap();
            if timers.iter().filter(|t| t.agent == agent).count() >= MAX_TIMERS_PER_AGENT {
                return Err(format!("you already have {MAX_TIMERS_PER_AGENT} pending wake-ups"));
            }
            timers.push(Timer {
                id: self.next_timer.fetch_add(1, Ordering::Relaxed),
                agent: agent.to_string(),
                at: unix_now() + in_secs,
                prompt: prompt.to_string(),
            });
        }
        self.save_timers();
        Ok(())
    }

    fn transcribe(&self, audio: &[u8], lang: Option<&str>) -> Result<String, String> {
        self.speech.transcribe(audio, lang)
    }

    fn speak(&self, text: &str, voice: Option<&str>) -> Result<(Vec<u8>, u64), String> {
        self.speech.speak(text, voice).map(|s| (s.audio, s.duration_ms))
    }

    fn save_agent(&self, spec: AgentSpec) -> Result<String, String> {
        if self.store.get(&spec.name).is_some() {
            self.update_agent(spec)?;
            Ok("updated".into())
        } else {
            self.create_agent(spec)?;
            Ok("created".into())
        }
    }

    fn delete_agent(&self, name: &str) -> Result<(), String> {
        Runtime::delete_agent(self, name)
    }

    fn pause_agent(&self, name: &str, paused: bool) -> Result<(), String> {
        self.arc()?.set_paused(name, paused)
    }

    fn embed(&self, texts: &[String], kind: &str) -> Option<Vec<Vec<f32>>> {
        self.embedder.as_ref()?.embed(texts, kind)
    }

    fn observe(&self, event: Event) {
        if let (Event::RunFinished(rec), Some(endpoint)) = (&event, &self.cfg.otlp_endpoint) {
            crate::otlp::export_async(endpoint.clone(), (**rec).clone());
        }
        self.observers.lock().unwrap().retain(|tx| tx.send(event.clone()).is_ok());
    }
}
