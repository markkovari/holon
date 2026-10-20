//! One per state directory: runs agents, fires their triggers, holds approvals open.

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
use crate::session::{self, AgentdConfig};
use crate::spec::{validate_name, validate_topic, AgentSpec, Trigger};
use crate::store::{RunRecord, Status, Store};
use crate::trace::{new_span_id, new_trace_id};

#[derive(Clone)]
pub struct Config {
    pub state_dir: std::path::PathBuf,
    /// Settable later: a console starts the runtime before its model server is up.
    pub local: LocalModel,
    /// Unanswered by then, a sensitive tool call is denied.
    pub approval_timeout: Duration,
    pub breaker_threshold: u32,
    pub breaker_cooldown: Duration,
    /// OTLP/HTTP base URL; every finished run goes to `<endpoint>/v1/traces`.
    pub otlp_endpoint: Option<String>,
    pub speech: crate::speech::SpeechConfig,
    /// Empty: memories are recalled by shared words.
    pub embed_url: String,
    /// Started on demand (`lazy.rs`).
    pub embed_service: String,
    pub google: crate::google::GoogleConfig,
    pub agentd: Option<AgentdConfig>,
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
            google: Default::default(),
            agentd: None,
        }
    }
}

pub use agent_wire::Pending;

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
    /// So the reminder is delivered back to that project.
    #[serde(default)]
    project: Option<String>,
}

const MAX_TIMERS_PER_AGENT: usize = 20;
const MAX_TASKS_KEPT: usize = 200;

pub struct Runtime {
    /// `Host` methods only have `&self` but hand an owned handle to the threads they spawn.
    me: std::sync::Weak<Runtime>,
    store: Store,
    kv: Kv,
    bus: Box<dyn Bus>,
    cfg: Config,
    speech: crate::speech::Speech,
    embedder: Option<crate::embed::Embedder>,
    google: crate::google::Google,
    jev: Option<crate::jev::JevClient>,
    local: Mutex<LocalModel>,
    started: u64,
    busy: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    approvals: Mutex<Vec<Slot>>,
    approvals_cv: Condvar,
    pub(crate) next_approval: AtomicU64,
    pub(crate) agentd: Option<session::Agentd>,
    pub(crate) sessions: Mutex<Vec<session::Tracked>>,
    observers: Mutex<Vec<Sender<Event>>>,
    mock_cursor: Mutex<HashMap<String, usize>>,
    last_fired: Mutex<HashMap<(String, usize), u64>>,
    stop: AtomicBool,
    tasks: Mutex<Vec<(String, TaskState)>>,
    next_task: AtomicU64,
    timers: Mutex<Vec<Timer>>,
    next_timer: AtomicU64,
    /// A second kick is folded into the drain in flight.
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
    spec.sessions.validate()?;
    for t in &spec.topics_out {
        validate_topic(&t.replace('*', "x")).map_err(|e| format!("topics_out: {e}"))?;
    }
    for ns in spec.store.read.iter().chain(&spec.store.write) {
        validate_topic(ns).map_err(|e| format!("store: {e}"))?;
    }
    for t in &spec.triggers {
        match t {
            Trigger::Schedule { cron, prompt, project, .. } => {
                cron::parse(cron)?;
                if let Some(p) = project {
                    validate_name(p).map_err(|e| format!("schedule project: {e}"))?;
                }
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

const CALL_WAIT: std::time::Duration = std::time::Duration::from_secs(90);

fn needs_better_model(rec: &RunRecord) -> bool {
    (rec.status == Status::Failed && rec.answer.starts_with("never called required tool"))
        || (rec.status == Status::Ok && rec.answer.trim().is_empty())
}

thread_local! {
    /// A call runs on its caller's thread, so this is the call stack of agents, outermost first.
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
        let cfg_agentd = match &cfg.agentd {
            Some(c) => Some(session::Agentd::new(c.clone())?),
            None => None,
        };
        let rt = Arc::new_cyclic(|me| Self {
            me: me.clone(),
            store,
            kv,
            bus: Box::new(bus),
            local: Mutex::new(cfg.local.clone()),
            speech: crate::speech::Speech::new(cfg.speech.clone()),
            embedder: crate::embed::Embedder::new(&cfg.embed_url, &cfg.embed_service),
            google: crate::google::Google::new(cfg.google.clone()),
            jev: crate::jev::JevClient::from_env(),
            cfg,
            started: unix_now(),
            busy: Mutex::new(HashMap::new()),
            approvals: Mutex::new(Vec::new()),
            approvals_cv: Condvar::new(),
            next_approval: AtomicU64::new(1),
            agentd: cfg_agentd,
            sessions: Mutex::new(Vec::new()),
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

    pub fn trace_otlp(&self, trace_id: &str) -> Value {
        crate::otlp::request(&self.store.trace(trace_id))
    }

    /// A new subscriber hears what happens from now on, not the topic's whole history.
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

    pub fn update_agent(&self, spec: AgentSpec) -> Result<(), String> {
        validate_spec(&spec)?;
        if self.store.get(&spec.name).is_none() {
            return Err(format!("no agent named {}", spec.name));
        }
        self.store.put(&spec)?;
        self.ensure_offsets(&spec);
        Ok(())
    }

    pub fn set_paused(self: &Arc<Self>, name: &str, paused: bool) -> Result<(), String> {
        let mut s = self.store.get(name).ok_or_else(|| format!("no agent named {name}"))?;
        s.paused = paused;
        self.store.put(&s)?;
        if !paused {
            self.kick(name);
        }
        Ok(())
    }

    pub fn put_project(&self, project: crate::projects::Project) -> Result<(), String> {
        project.validate()?;
        if let Some(missing) = project.agents.iter().find(|a| self.store.get(a).is_none()) {
            return Err(format!("no agent named {missing}"));
        }
        self.store.put_project(&project)
    }

    /// Unarchived first, so its agents are not left paused with nothing to say why.
    pub fn delete_project(self: &Arc<Self>, name: &str) -> Result<(), String> {
        let _ = self.unarchive_project(name);
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

    pub fn remove_from_project(&self, name: &str, agent: &str) -> Result<(), String> {
        let mut p =
            self.store.get_project(name).ok_or_else(|| format!("no project named {name}"))?;
        p.agents.retain(|a| a != agent);
        p.paused_by_archive.retain(|a| a != agent);
        if p.lead.as_deref() == Some(agent) {
            p.lead = None;
        }
        self.put_project(p)
    }

    /// A member already paused is the owner's doing: it is left alone and not recorded.
    pub fn archive_project(self: &Arc<Self>, name: &str) -> Result<Vec<String>, String> {
        let mut p =
            self.store.get_project(name).ok_or_else(|| format!("no project named {name}"))?;
        if p.archived {
            return Ok(Vec::new());
        }
        let others = self.store.list_projects();
        let mut paused = Vec::new();
        for a in &p.agents {
            let live_elsewhere =
                others.iter().any(|q| q.name != p.name && !q.archived && q.agents.contains(a));
            if live_elsewhere || self.store.get(a).is_none_or(|s| s.paused) {
                continue;
            }
            self.set_paused(a, true)?;
            paused.push(a.clone());
        }
        p.archived = true;
        p.paused_by_archive = paused.clone();
        self.store.put_project(&p)?;
        Ok(paused)
    }

    /// Resumes members any project's record says an archive paused, whichever project that was.
    pub fn unarchive_project(self: &Arc<Self>, name: &str) -> Result<Vec<String>, String> {
        let mut p =
            self.store.get_project(name).ok_or_else(|| format!("no project named {name}"))?;
        if !p.archived {
            return Ok(Vec::new());
        }
        p.archived = false;
        let mut all = self.store.list_projects();
        let recorded: std::collections::HashSet<String> =
            all.iter().flat_map(|q| q.paused_by_archive.clone()).collect();
        let resume: Vec<String> =
            p.agents.iter().filter(|a| recorded.contains(*a)).cloned().collect();
        for a in &resume {
            if self.store.get(a).is_some_and(|s| s.paused) {
                self.set_paused(a, false)?;
            }
        }
        for q in all.iter_mut().filter(|q| q.name != p.name) {
            let before = q.paused_by_archive.len();
            q.paused_by_archive.retain(|a| !resume.contains(a));
            if q.paused_by_archive.len() != before {
                self.store.put_project(q)?;
            }
        }
        p.paused_by_archive.retain(|a| !resume.contains(a));
        self.store.put_project(&p)?;
        Ok(resume)
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

    /// Its timers are not restored; the schedules in its spec are.
    pub fn restore_agent(&self, id_or_name: &str) -> Result<crate::store::Trashed, String> {
        let t = self.store.restore(id_or_name)?;
        for p in &t.projects {
            let _ = self.add_to_project(p, &t.name);
        }
        Ok(t)
    }

    fn lock_for(&self, name: &str) -> Arc<Mutex<()>> {
        self.busy.lock().unwrap().entry(name.to_string()).or_default().clone()
    }

    fn admit(&self, spec: &AgentSpec, cause: &Cause, via: &str) -> Result<(), String> {
        let now = unix_now();
        if cause.hops > MAX_HOPS {
            return Err(format!("chain is {} wake-ups deep; the limit is {MAX_HOPS}", cause.hops));
        }
        if let Some(p) = &cause.project {
            // `spec` is the effective one: its projects are the ones the agent is in.
            if !spec.projects.iter().any(|x| &x.name == p) {
                return Err(format!("{} is not a member of the project {p}", spec.name));
            }
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

    /// A refused wake-up still leaves a record: silence would look like the agent ignoring it.
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
            project: cause.project.clone().unwrap_or_default(),
            attachments: cause.attachments.clone(),
        };
        let _ = self.store.record_run(&rec);
        self.observe(Event::RunFinished(Box::new(rec)));
    }

    /// Without `wait` a busy agent is an error: a schedule tick should skip, not pile up.
    /// `via` (topic or `call`) feeds cycle detection; empty for triggers that cannot cycle.
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
        let directory: Vec<(String, String)> =
            self.store.list().into_iter().map(|a| (a.name, a.description)).collect();
        let mut spec = crate::projects::effective(&spec, &self.store.list_projects(), &directory);
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
            // An agent up this call stack waits on us: queueing behind it would deadlock.
            if RUNNING_HERE.with(|r| r.borrow().iter().any(|n| n == name)) {
                return Err(format!("{name} is already waiting on this call"));
            }
            // Bounded, so two agents waiting on each other time out instead of hanging.
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
        let mut c = cause.clone();
        if !via.is_empty() {
            c.chain.push(format!("{name}|{via}"));
        }
        RUNNING_HERE.with(|r| r.borrow_mut().push(name.to_string()));
        let mut rec = agent::run(self, &spec, trigger, input, &c);
        // Retried on the next fallback model only on a miss, so a good hosted answer never
        // wakes a local server.
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
                // Kicked even when paused: the drain refuses then, and resuming kicks again.
                self.kick(&spec.name);
            }
        }
        Ok(woken)
    }

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
            // Something may have been published between the last empty check and the flag
            // clearing; look once more.
            if rt.next_pending(&agent).is_none()
                || !rt.draining.lock().unwrap().insert(agent.clone())
            {
                break;
            }
        });
    }

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
            project: None,
            attachments: Vec::new(),
        };
        let _ = self.run_agent(agent, &label, &env.draft.payload, &cause, topic, true);
    }

    pub fn recover(self: &Arc<Self>) {
        for spec in self.store.list() {
            if spec.triggers.iter().any(|t| t.topic().is_some()) {
                self.kick(&spec.name);
            }
        }
    }

    fn save_timers(&self) {
        let t = self.timers.lock().unwrap();
        let _ = std::fs::write(
            self.cfg.state_dir.join("timers.json"),
            serde_json::to_string(&*t).unwrap_or_default(),
        );
    }

    /// Public so a test can drive it without waiting on the thread.
    pub fn tick(self: &Arc<Self>, now: u64) -> usize {
        let mut fired = 0;
        for spec in self.store.list() {
            if spec.paused {
                continue;
            }
            for (i, t) in spec.triggers.iter().enumerate() {
                let Trigger::Schedule { cron: expr, prompt, project, .. } = t else { continue };
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
                    let cause = Cause { project: project.clone(), ..Cause::default() };
                    std::thread::spawn(move || {
                        let _ = rt.run_agent(&name, &trig, &input, &cause, "", false);
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
                let cause = Cause { project: t.project.clone(), ..Cause::default() };
                let _ = rt.run_agent(&t.agent, "timer", &t.prompt, &cause, "", true);
            });
        }
        fired
    }

    pub fn start_scheduler(self: &Arc<Self>) {
        self.recover();
        self.start_session_watcher();
        let rt = self.clone();
        std::thread::spawn(move || {
            let mut swept = 0;
            while !rt.stop.load(Ordering::Relaxed) {
                let now = unix_now();
                rt.tick(now);
                if now.saturating_sub(swept) >= 3600 {
                    rt.store.sweep_trash(now);
                    swept = now;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.close_all_sessions();
    }

    pub(crate) fn is_stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

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

    pub fn pending_approvals(&self) -> Vec<Pending> {
        let mut all: Vec<Pending> = self
            .approvals
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.answer.is_none())
            .map(|s| s.pending.clone())
            .collect();
        all.extend(self.session_pending());
        all
    }

    /// `false`: already answered or timed out.
    pub fn resolve_approval(&self, id: u64, approved: bool) -> bool {
        self.resolve_approval_scoped(id, approved, "once")
    }

    /// `scope` (ADR-0114) reaches only a coding session's tool calls.
    pub fn resolve_approval_scoped(&self, id: u64, approved: bool, scope: &str) -> bool {
        {
            let mut list = self.approvals.lock().unwrap();
            if let Some(s) = list.iter_mut().find(|s| s.pending.id == id && s.answer.is_none()) {
                s.answer = Some(approved);
                self.approvals_cv.notify_all();
                return true;
            }
        }
        self.resolve_session_approval(id, approved, scope).unwrap_or(false)
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
        cause: &Cause,
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
                project: cause.project.clone(),
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

    fn add_to_project(&self, project: &str, agent: &str) -> Result<(), String> {
        Runtime::add_to_project(self, project, agent)
    }

    fn google(&self) -> Option<&crate::google::Google> {
        Some(&self.google)
    }

    fn jev(&self) -> Option<&dyn crate::jev::Jev> {
        self.jev.as_ref().map(|j| j as &dyn crate::jev::Jev)
    }

    fn restore_agent(&self, name: &str) -> Result<String, String> {
        let t = Runtime::restore_agent(self, name)?;
        let joined = if t.projects.is_empty() {
            String::new()
        } else {
            format!(", back in its projects ({})", t.projects.join(", "))
        };
        Ok(format!(
            "restored {} from the trash with its settings, schedules, {} recorded runs, memory \
             and workspace{joined}; it runs on its schedules again",
            t.name, t.runs
        ))
    }

    fn pause_agent(&self, name: &str, paused: bool) -> Result<(), String> {
        self.arc()?.set_paused(name, paused)
    }

    fn embed(&self, texts: &[String], kind: &str) -> Option<Vec<Vec<f32>>> {
        self.embedder.as_ref()?.embed(texts, kind)
    }

    fn session_start(
        &self,
        spec: &AgentSpec,
        run_id: &str,
        cause: &Cause,
        task: &str,
        workspace: &str,
    ) -> Result<String, String> {
        self.start_session(spec, run_id, cause, task, workspace)
    }

    fn session_result(
        &self,
        agent: &str,
        id: &str,
        wait: Duration,
    ) -> Result<session::SessionView, String> {
        Runtime::session_result(self, agent, id, wait)
    }

    fn session_close(&self, agent: &str, id: &str) -> Result<(), String> {
        self.close_session(agent, id)
    }

    fn observe(&self, event: Event) {
        if let (Event::RunFinished(rec), Some(endpoint)) = (&event, &self.cfg.otlp_endpoint) {
            crate::otlp::export_async(endpoint.clone(), (**rec).clone());
        }
        self.observers.lock().unwrap().retain(|tx| tx.send(event.clone()).is_ok());
    }
}
