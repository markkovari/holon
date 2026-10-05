//! The runtime: owns the store, runs agents, fires their triggers, and holds
//! approvals open while a human decides. One per state directory.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::agent::{self, Event, Host, MAX_DEPTH};
use crate::cron;
use crate::model::{self, LocalModel, Msg, Reply};
use crate::spec::{validate_name, AgentSpec, Trigger};
use crate::store::{RunRecord, Store};

#[derive(Clone)]
pub struct Config {
    pub state_dir: std::path::PathBuf,
    /// Where `ModelSpec::Local` points. Settable later (`set_local_model`)
    /// because a console starts the runtime before its model server is up.
    pub local: LocalModel,
    /// How long a sensitive tool call waits for a human before it is denied.
    pub approval_timeout: Duration,
}

impl Config {
    pub fn new(state_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            local: LocalModel::default(),
            approval_timeout: Duration::from_secs(300),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Pending {
    pub id: u64,
    pub agent: String,
    pub tool: String,
    pub args: Value,
}

struct Slot {
    pending: Pending,
    answer: Option<bool>,
}

pub struct Runtime {
    /// Weak self, so `Host::emit` (which only has `&self`) can hand an owned
    /// handle to the threads it spawns.
    me: std::sync::Weak<Runtime>,
    store: Store,
    cfg: Config,
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
}

pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Checked before a spec is saved, so a typo in a cron expression is an error
/// now rather than an agent that silently never fires.
pub fn validate_spec(spec: &AgentSpec) -> Result<(), String> {
    validate_name(&spec.name)?;
    if spec.description.trim().is_empty() {
        return Err("description is empty — it is the agent's purpose and its system prompt".into());
    }
    if !(1..=50).contains(&spec.max_steps) {
        return Err("max_steps must be 1-50".into());
    }
    let mut seen = std::collections::HashSet::new();
    for c in &spec.capabilities {
        if c.name.is_empty() || !seen.insert(c.name.clone()) {
            return Err(format!("capability `{}` is empty or listed twice", c.name));
        }
        if let Some(t) = c.name.strip_prefix("agent:") {
            validate_name(t)?;
        }
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
            Trigger::Event { topic } if topic.trim().is_empty() => {
                return Err("event topic is empty".into())
            }
            Trigger::Event { .. } => {}
        }
    }
    Ok(())
}

impl Runtime {
    pub fn new(cfg: Config) -> Result<Arc<Self>, String> {
        let store = Store::open(&cfg.state_dir)?;
        Ok(Arc::new_cyclic(|me| Self {
            me: me.clone(),
            store,
            local: Mutex::new(cfg.local.clone()),
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
        }))
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn set_local_model(&self, m: LocalModel) {
        *self.local.lock().unwrap() = m;
    }

    pub fn subscribe(&self) -> Receiver<Event> {
        let (tx, rx) = channel();
        self.observers.lock().unwrap().push(tx);
        rx
    }

    // ---- lifecycle --------------------------------------------------------

    pub fn create_agent(&self, spec: AgentSpec) -> Result<(), String> {
        validate_spec(&spec)?;
        if self.store.get(&spec.name).is_some() {
            return Err(format!("{} already exists", spec.name));
        }
        self.store.put(&spec)
    }

    /// Replaces the spec. The next run uses it — no rebuild, no redeploy.
    pub fn update_agent(&self, spec: AgentSpec) -> Result<(), String> {
        validate_spec(&spec)?;
        if self.store.get(&spec.name).is_none() {
            return Err(format!("no agent named {}", spec.name));
        }
        self.store.put(&spec)
    }

    pub fn set_paused(&self, name: &str, paused: bool) -> Result<(), String> {
        let mut s = self.store.get(name).ok_or_else(|| format!("no agent named {name}"))?;
        s.paused = paused;
        self.store.put(&s)
    }

    pub fn delete_agent(&self, name: &str) -> Result<(), String> {
        self.store.delete(name)?;
        self.last_fired.lock().unwrap().retain(|(n, _), _| n != name);
        Ok(())
    }

    // ---- running ----------------------------------------------------------

    fn lock_for(&self, name: &str) -> Arc<Mutex<()>> {
        self.busy.lock().unwrap().entry(name.to_string()).or_default().clone()
    }

    /// Runs `name` to completion. One run per agent at a time: with `wait`
    /// it queues behind the current one, without it a busy agent is an error
    /// (a schedule tick that finds its agent still working should skip, not
    /// pile up behind it).
    pub fn run_agent(
        &self,
        name: &str,
        trigger: &str,
        input: &str,
        depth: u32,
        wait: bool,
    ) -> Result<RunRecord, String> {
        let spec = self.store.get(name).ok_or_else(|| format!("no agent named {name}"))?;
        if spec.paused {
            return Err(format!("{name} is paused"));
        }
        let lock = self.lock_for(name);
        let _guard = if wait {
            lock.lock().unwrap()
        } else {
            lock.try_lock().map_err(|_| format!("{name} is busy with another run"))?
        };
        Ok(agent::run(self, &spec, trigger, input, depth))
    }

    /// Wakes every unpaused agent subscribed to `topic`, each on its own
    /// thread. Returns how many it woke. Chains deeper than `MAX_DEPTH` are
    /// dropped, which is what stops two agents ping-ponging events forever.
    pub fn emit_event(self: &Arc<Self>, topic: &str, payload: &str, depth: u32) -> usize {
        if depth > MAX_DEPTH {
            return 0;
        }
        let mut woken = 0;
        for spec in self.store.list() {
            let subscribed = !spec.paused
                && spec
                    .triggers
                    .iter()
                    .any(|t| matches!(t, Trigger::Event { topic: t } if t == topic));
            if subscribed {
                woken += 1;
                let (rt, name, trig, input) =
                    (self.clone(), spec.name, format!("event: {topic}"), payload.to_string());
                std::thread::spawn(move || {
                    let _ = rt.run_agent(&name, &trig, &input, depth, true);
                });
            }
        }
        woken
    }

    // ---- schedule ---------------------------------------------------------

    /// Evaluates every schedule trigger once, firing what is due. Public so a
    /// test can drive it without waiting on the background thread.
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
                        let _ = rt.run_agent(&name, &trig, &input, 0, false);
                    });
                }
            }
        }
        fired
    }

    /// Starts the background scheduler: checks once a second.
    pub fn start_scheduler(self: &Arc<Self>) {
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
}

impl Host for Runtime {
    fn now(&self) -> u64 {
        unix_now()
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

    fn approve(&self, agent: &str, tool: &str, args: &Value) -> bool {
        let id = self.next_approval.fetch_add(1, Ordering::Relaxed);
        let pending =
            Pending { id, agent: agent.to_string(), tool: tool.to_string(), args: args.clone() };
        self.approvals.lock().unwrap().push(Slot { pending, answer: None });
        self.observe(Event::ApprovalRequested {
            id,
            agent: agent.to_string(),
            tool: tool.to_string(),
            args: args.clone(),
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
        depth: u32,
    ) -> Result<String, String> {
        if depth > MAX_DEPTH {
            return Err("delegation is nested too deeply".into());
        }
        if target == caller {
            return Err("an agent cannot delegate to itself".into());
        }
        let r = self.run_agent(target, &format!("agent: {caller}"), message, depth, false)?;
        match r.status {
            crate::store::Status::Ok => Ok(r.answer),
            _ => Err(format!("{target} failed: {}", r.answer)),
        }
    }

    fn emit(&self, _from: &str, topic: &str, payload: &str, depth: u32) {
        if let Some(rt) = self.me.upgrade() {
            rt.emit_event(topic, payload, depth);
        }
    }

    fn observe(&self, event: Event) {
        self.observers.lock().unwrap().retain(|tx| tx.send(event.clone()).is_ok());
    }
}
