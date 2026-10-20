//! Parents are known, not guessed: a run announces each tool call's span before making it, and what
//! the call wakes names that span. No window and no clock, so every rule here is a test.

use std::collections::HashMap;

use agent_wire::{Event, Phase, RunRecord, SessionView, Status as RunStatus, Step};
use serde_json::Value;

pub const KEEP_TREE_MS: u64 = 6 * 3_600_000;
/// A finished tree of one node is a chat reply or a schedule that fired: it goes sooner.
pub const KEEP_LONE_MS: u64 = 90_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Running,
    NeedsYou,
    Done,
    Failed,
    Stopped,
}

impl Status {
    pub fn is_over(self) -> bool {
        matches!(self, Status::Done | Status::Failed | Status::Stopped)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Run,
    Session,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub id: u64,
    pub tool: String,
    pub args: Value,
    /// A coding session's tool call, which can be approved beyond one call.
    pub session: bool,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub id: String,
    pub kind: Kind,
    pub name: String,
    pub model: String,
    pub task: String,
    pub status: Status,
    pub tools: Vec<String>,
    pub activity: String,
    pub tokens: u64,
    pub cost_micros: Option<u64>,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    pub result: Option<String>,
    pub parent: Option<String>,
    pub children: Vec<String>,
    pub pending: Vec<Pending>,
    /// When the board first saw it: what the enter animation is timed from.
    pub born_ms: u64,
    pub notes: Vec<String>,
    tokens_in: u64,
    tokens_out: u64,
}

impl Node {
    fn new(id: &str, kind: Kind, name: &str, now: u64) -> Self {
        Self {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            model: String::new(),
            task: String::new(),
            status: Status::Running,
            tools: Vec::new(),
            activity: String::new(),
            tokens: 0,
            cost_micros: Some(0),
            started_ms: now,
            finished_ms: None,
            result: None,
            parent: None,
            children: Vec::new(),
            pending: Vec::new(),
            born_ms: now,
            notes: Vec::new(),
            tokens_in: 0,
            tokens_out: 0,
        }
    }

    pub fn elapsed_ms(&self, now: u64) -> u64 {
        self.finished_ms.unwrap_or(now).saturating_sub(self.started_ms)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub agents: usize,
    pub running: usize,
    pub waiting: Vec<Waiting>,
    pub cost_micros: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionRow {
    pub root: String,
    pub title: String,
    pub lead: String,
    pub status: Status,
    pub agents: usize,
    pub waiting: usize,
    pub cost_micros: u64,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Waiting {
    pub node: String,
    pub agent: String,
    pub via: Vec<String>,
    pub pending: Pending,
}

#[derive(Default)]
pub struct AgentTree {
    nodes: HashMap<String, Node>,
    order: Vec<String>,
    /// a call's span -> the id of the run making it
    spans: HashMap<String, String>,
    approvals: HashMap<u64, String>,
    /// Changes so far: a view that remembers it saw revision N knows whether it is stale.
    pub revision: u64,
}

fn brief(args: &Value) -> String {
    for k in ["path", "command", "url", "query", "message", "task", "text", "topic", "key"] {
        if let Some(s) = args.get(k).and_then(Value::as_str) {
            let line: String = s.lines().next().unwrap_or("").chars().take(48).collect();
            return line;
        }
    }
    String::new()
}

fn note(notes: &mut Vec<String>, v: &Value) {
    match v {
        Value::String(s) if !s.is_empty() && !notes.contains(s) => {
            notes.push(s.chars().take(400).collect())
        }
        Value::Array(a) => a.iter().for_each(|x| note(notes, x)),
        Value::Object(o) => o.values().for_each(|x| note(notes, x)),
        _ => {}
    }
}

fn activity_of(tool: &str, args: &Value) -> String {
    match tool.strip_prefix("agent:") {
        Some(other) => format!("asking {other}"),
        None => format!("{tool} {}", brief(args)).trim().to_string(),
    }
}

/// Micro-USD; `None` for a model with no known price, since a made-up one would be worse.
pub fn price(model: &str, tokens_in: u64, tokens_out: u64) -> Option<u64> {
    if model.starts_with("local/") || model.starts_with("mock") || model.is_empty() {
        return Some(0);
    }
    let known = ["deepseek", "haiku", "sonnet", "opus", "claude"];
    known
        .iter()
        .any(|k| model.contains(k))
        .then(|| comp_fuel::cost::cost_usd_micros(tokens_in, tokens_out, 0, 0, model, false))
}

fn run_status(s: RunStatus) -> Status {
    match s {
        RunStatus::Ok => Status::Done,
        RunStatus::Failed => Status::Failed,
        RunStatus::OverBudget | RunStatus::Dropped => Status::Stopped,
    }
}

fn session_status(p: Phase) -> Status {
    match p {
        Phase::Running => Status::Running,
        Phase::NeedsYou => Status::NeedsYou,
        Phase::Done => Status::Done,
        Phase::Failed => Status::Failed,
        Phase::Cancelled => Status::Stopped,
    }
}

impl AgentTree {
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.get(id)
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn all(&self) -> impl Iterator<Item = &Node> {
        self.order.iter().filter_map(|id| self.nodes.get(id))
    }

    pub fn roots(&self) -> Vec<&Node> {
        self.order
            .iter()
            .filter_map(|id| self.nodes.get(id))
            .filter(|n| n.parent.is_none())
            .collect()
    }

    pub fn children_of(&self, id: &str) -> Vec<&Node> {
        self.nodes
            .get(id)
            .map(|n| n.children.iter().filter_map(|c| self.nodes.get(c)).collect())
            .unwrap_or_default()
    }

    pub fn ancestors(&self, id: &str) -> Vec<String> {
        let mut names = Vec::new();
        let mut at = self.nodes.get(id).and_then(|n| n.parent.clone());
        while let Some(p) = at {
            let Some(n) = self.nodes.get(&p) else { break };
            names.push(n.name.clone());
            at = n.parent.clone();
        }
        names.reverse();
        names
    }

    fn subtree(&self, id: &str, out: &mut Vec<String>) {
        out.push(id.to_string());
        if let Some(n) = self.nodes.get(id) {
            for c in &n.children {
                self.subtree(c, out);
            }
        }
    }

    pub fn visible_roots(&self, now: u64) -> Vec<&Node> {
        let mut shown: Vec<&Node> = self
            .roots()
            .into_iter()
            .filter(|r| {
                let mut ids = Vec::new();
                self.subtree(&r.id, &mut ids);
                let nodes: Vec<&Node> = ids.iter().filter_map(|i| self.nodes.get(i)).collect();
                if nodes.iter().any(|n| !n.status.is_over()) {
                    return true;
                }
                let last = nodes.iter().filter_map(|n| n.finished_ms).max().unwrap_or(now);
                let keep = if nodes.len() > 1 { KEEP_TREE_MS } else { KEEP_LONE_MS };
                now.saturating_sub(last) < keep
            })
            .collect();
        // newest first; of two that began in the same millisecond, the one seen later
        shown.reverse();
        shown.sort_by_key(|n| std::cmp::Reverse(n.started_ms));
        shown
    }

    pub fn sessions(&self, now: u64) -> Vec<SessionRow> {
        self.visible_roots(now)
            .into_iter()
            .map(|r| {
                let mut ids = Vec::new();
                self.subtree(&r.id, &mut ids);
                let nodes: Vec<&Node> = ids.iter().filter_map(|i| self.nodes.get(i)).collect();
                let waiting: usize = nodes.iter().map(|n| n.pending.len()).sum();
                let over = nodes.iter().all(|n| n.status.is_over());
                let status = if nodes.iter().any(|n| n.status == Status::NeedsYou) {
                    Status::NeedsYou
                } else if !over {
                    Status::Running
                } else {
                    r.status
                };
                SessionRow {
                    root: r.id.clone(),
                    title: r.task.clone(),
                    lead: r.name.clone(),
                    status,
                    agents: nodes.len(),
                    waiting,
                    cost_micros: nodes.iter().map(|n| n.cost_micros.unwrap_or(0)).sum(),
                    started_ms: r.started_ms,
                    finished_ms: if over {
                        nodes.iter().filter_map(|n| n.finished_ms).max()
                    } else {
                        None
                    },
                }
            })
            .collect()
    }

    pub fn texts_of(&self, root: &str) -> Vec<String> {
        let mut ids = Vec::new();
        self.subtree(root, &mut ids);
        ids.iter()
            .filter_map(|i| self.nodes.get(i))
            .flat_map(|n| {
                std::iter::once(n.task.clone())
                    .chain(n.result.clone())
                    .chain(n.notes.iter().cloned())
            })
            .filter(|t| !t.is_empty())
            .collect()
    }

    pub fn summary_of(&self, root: &str) -> Summary {
        let mut s = Summary::default();
        let mut ids = Vec::new();
        self.subtree(root, &mut ids);
        for id in ids {
            let Some(n) = self.nodes.get(&id) else { continue };
            s.agents += 1;
            if matches!(n.status, Status::Running | Status::NeedsYou) {
                s.running += 1;
            }
            s.cost_micros += n.cost_micros.unwrap_or(0);
            let via = self.ancestors(&n.id);
            for p in &n.pending {
                s.waiting.push(Waiting {
                    node: n.id.clone(),
                    agent: n.name.clone(),
                    via: via.clone(),
                    pending: p.clone(),
                });
            }
        }
        s
    }

    pub fn seed(&mut self, runs: &[RunRecord]) {
        let mut runs: Vec<&RunRecord> = runs.iter().collect();
        runs.sort_by_key(|r| r.started_ms);
        for r in &runs {
            for step in &r.steps {
                if let Step::Tool { span_id, .. } = step {
                    self.spans.insert(span_id.clone(), r.id.clone());
                }
            }
        }
        for r in runs {
            if self.nodes.contains_key(&r.id) {
                continue;
            }
            let parent = r.parent_span_id.as_ref().and_then(|s| self.spans.get(s)).cloned();
            let mut n = Node::new(&r.id, Kind::Run, &r.agent, r.started_ms);
            n.model = r.model.clone();
            n.task = r.input.clone();
            n.status = run_status(r.status);
            n.tokens_in = r.tokens_in;
            n.tokens_out = r.tokens_out;
            n.tokens = r.tokens_in + r.tokens_out;
            n.cost_micros = price(&n.model, n.tokens_in, n.tokens_out);
            for step in &r.steps {
                if let Step::Tool { name, args, .. } = step {
                    if !n.tools.contains(name) && !name.starts_with("agent:") {
                        n.tools.push(name.clone());
                    }
                    note(&mut n.notes, args);
                }
            }
            n.finished_ms = Some(r.finished_ms.max(r.started_ms));
            n.result = Some(r.answer.clone());
            self.insert(n, parent);
        }
        self.revision += 1;
    }

    pub fn summary(&self, now: u64) -> Summary {
        let mut s = Summary::default();
        for root in self.visible_roots(now) {
            let mut ids = Vec::new();
            self.subtree(&root.id, &mut ids);
            for id in ids {
                let Some(n) = self.nodes.get(&id) else { continue };
                s.agents += 1;
                if n.status == Status::Running || n.status == Status::NeedsYou {
                    s.running += 1;
                }
                s.cost_micros += n.cost_micros.unwrap_or(0);
                let via = self.ancestors(&n.id);
                for p in &n.pending {
                    s.waiting.push(Waiting {
                        node: n.id.clone(),
                        agent: n.name.clone(),
                        via: via.clone(),
                        pending: p.clone(),
                    });
                }
            }
        }
        s
    }

    fn insert(&mut self, mut node: Node, parent: Option<String>) {
        if self.nodes.contains_key(&node.id) {
            return;
        }
        node.parent = parent.clone().filter(|p| self.nodes.contains_key(p));
        if let Some(p) = &node.parent {
            if let Some(pn) = self.nodes.get_mut(p) {
                pn.children.push(node.id.clone());
            }
        }
        self.order.push(node.id.clone());
        self.nodes.insert(node.id.clone(), node);
    }

    fn adopt(&mut self, id: &str, parent: Option<String>) {
        let Some(parent) = parent.filter(|p| p != id && self.nodes.contains_key(p)) else { return };
        let old = self.nodes.get(id).and_then(|n| n.parent.clone());
        if old.as_deref() == Some(parent.as_str()) {
            return;
        }
        if let Some(o) = old.and_then(|o| self.nodes.get_mut(&o)) {
            o.children.retain(|c| c != id);
        }
        if let Some(p) = self.nodes.get_mut(&parent) {
            p.children.push(id.to_string());
        }
        if let Some(n) = self.nodes.get_mut(id) {
            n.parent = Some(parent);
        }
    }

    fn live_run_of(&self, agent: &str) -> Option<String> {
        self.order
            .iter()
            .rev()
            .filter_map(|id| self.nodes.get(id))
            .find(|n| n.kind == Kind::Run && n.name == agent && !n.status.is_over())
            .map(|n| n.id.clone())
    }

    pub fn apply(&mut self, ev: &Event, now: u64) {
        match ev {
            Event::RunStarted { agent, run_id, input, parent_span_id, model, .. } => {
                let parent = parent_span_id.as_ref().and_then(|s| self.spans.get(s)).cloned();
                let mut n = Node::new(run_id, Kind::Run, agent, now);
                n.model = model.clone();
                n.task = input.clone();
                n.cost_micros = price(model, 0, 0);
                self.insert(n, parent);
            }
            Event::ToolStarted { run_id, span_id, tool, args, .. } => {
                self.spans.insert(span_id.clone(), run_id.clone());
                if let Some(n) = self.nodes.get_mut(run_id) {
                    if !n.tools.contains(tool) && !tool.starts_with("agent:") {
                        n.tools.push(tool.clone());
                    }
                    n.activity = activity_of(tool, args);
                    note(&mut n.notes, args);
                }
            }
            Event::StepDone { run_id, step, .. } => {
                if let Some(n) = self.nodes.get_mut(run_id) {
                    match step {
                        Step::Model { tokens_in, tokens_out, .. } => {
                            n.tokens_in += tokens_in;
                            n.tokens_out += tokens_out;
                            n.tokens = n.tokens_in + n.tokens_out;
                            n.cost_micros = price(&n.model, n.tokens_in, n.tokens_out);
                            n.activity = "thinking".into();
                        }
                        Step::Tool { .. } => n.activity.clear(),
                    }
                }
            }
            Event::RunFinished(rec) => {
                let parent = rec.parent_span_id.as_ref().and_then(|s| self.spans.get(s)).cloned();
                self.adopt(&rec.id, parent);
                if let Some(n) = self.nodes.get_mut(&rec.id) {
                    n.status = run_status(rec.status);
                    n.tokens_in = rec.tokens_in;
                    n.tokens_out = rec.tokens_out;
                    n.tokens = rec.tokens_in + rec.tokens_out;
                    if !rec.model.is_empty() {
                        n.model = rec.model.clone();
                    }
                    n.cost_micros = price(&n.model, n.tokens_in, n.tokens_out);
                    n.finished_ms = Some(if rec.finished_ms > 0 { rec.finished_ms } else { now });
                    n.result = Some(rec.answer.clone());
                    n.activity.clear();
                    n.pending.clear();
                }
            }
            Event::ApprovalRequested { id, agent, tool, args, chain } => {
                let session = chain.iter().find_map(|c| c.strip_prefix("session "));
                let node = match session {
                    Some(s) => Some(format!("session:{s}")),
                    None => self.live_run_of(agent),
                };
                if let Some(n) = node.as_ref().and_then(|n| self.nodes.get_mut(n)) {
                    if !n.pending.iter().any(|p| p.id == *id) {
                        n.pending.push(Pending {
                            id: *id,
                            tool: tool.clone(),
                            args: args.clone(),
                            session: session.is_some(),
                        });
                    }
                    if !n.status.is_over() {
                        n.status = Status::NeedsYou;
                    }
                    self.approvals.insert(*id, n.id.clone());
                }
            }
            Event::ApprovalResolved { id, .. } => {
                if let Some(n) = self.approvals.remove(id).and_then(|n| self.nodes.get_mut(&n)) {
                    n.pending.retain(|p| p.id != *id);
                    if n.pending.is_empty() && n.status == Status::NeedsYou {
                        n.status = Status::Running;
                    }
                }
            }
            Event::Session(view) => self.apply_session(view, now),
        }
        self.revision += 1;
    }

    fn apply_session(&mut self, v: &SessionView, now: u64) {
        let id = format!("session:{}", v.id);
        if !self.nodes.contains_key(&id) {
            let mut n = Node::new(&id, Kind::Session, "Coder", now);
            n.started_ms = v.started_ms;
            self.insert(n, Some(v.run_id.clone()));
        }
        let Some(n) = self.nodes.get_mut(&id) else { return };
        n.model = v.model.clone();
        n.task = v.task.clone();
        n.status = session_status(v.phase);
        n.tools = v.tools.clone();
        n.activity = v.activity.clone();
        note(&mut n.notes, &Value::String(v.activity.clone()));
        for p in &v.pending {
            note(&mut n.notes, &p.args);
        }
        n.tokens = v.tokens_in + v.tokens_out;
        n.cost_micros = Some(v.cost_micros);
        n.finished_ms = v.finished_ms;
        n.result = v.result.clone();
        n.pending = v
            .pending
            .iter()
            .map(|p| Pending {
                id: p.id,
                tool: p.tool.strip_prefix("session:").unwrap_or(&p.tool).to_string(),
                args: p.args.clone(),
                session: true,
            })
            .collect();
        for p in &n.pending {
            self.approvals.insert(p.id, id.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const T0: u64 = 1_000_000;

    fn started(agent: &str, run: &str, parent_span: Option<&str>, model: &str) -> Event {
        Event::RunStarted {
            agent: agent.into(),
            run_id: run.into(),
            trigger: "http".into(),
            input: format!("{agent}'s task"),
            trace_id: "t".into(),
            span_id: format!("span-{run}"),
            parent_span_id: parent_span.map(String::from),
            model: model.into(),
            chain: vec![],
        }
    }

    fn tool(run: &str, span: &str, name: &str, args: Value) -> Event {
        Event::ToolStarted {
            agent: "x".into(),
            run_id: run.into(),
            span_id: span.into(),
            tool: name.into(),
            args,
        }
    }

    fn model_step(run: &str, tin: u64, tout: u64) -> Event {
        Event::StepDone {
            agent: "x".into(),
            run_id: run.into(),
            step: Step::Model {
                text: String::new(),
                span_id: String::new(),
                t0: 0,
                t1: 0,
                tokens_in: tin,
                tokens_out: tout,
            },
        }
    }

    fn finished(run: &str, status: &str, at: u64) -> Event {
        let rec = serde_json::from_value(json!({
            "id": run, "agent": "x", "trigger": "http", "input": "", "started": 0,
            "finished": 0, "status": status, "answer": "the answer", "steps": [],
            "tokens_in": 1000, "tokens_out": 200, "finished_ms": at,
            "model": "anthropic/claude-haiku-4-5",
        }))
        .unwrap();
        Event::RunFinished(Box::new(rec))
    }

    #[test]
    fn an_agent_a_call_woke_hangs_under_the_call_that_woke_it() {
        let mut t = AgentTree::default();
        t.apply(&started("lead", "r1", None, "local/system"), T0);
        t.apply(&tool("r1", "call-1", "agent:researcher", json!({"message": "look"})), T0);
        t.apply(&started("researcher", "r2", Some("call-1"), "local/system"), T0);
        assert_eq!(t.roots().len(), 1);
        assert_eq!(t.children_of("r1").iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["r2"]);
        assert_eq!(t.node("r1").unwrap().activity, "asking researcher");
        assert!(t.node("r1").unwrap().tools.is_empty());
    }

    #[test]
    fn a_run_nobody_called_is_a_tree_of_its_own() {
        let mut t = AgentTree::default();
        t.apply(&started("a", "r1", None, ""), T0);
        t.apply(&started("b", "r2", Some("a-call-that-was-never-announced"), ""), T0);
        assert_eq!(t.roots().len(), 2);
    }

    #[test]
    fn tools_tokens_and_cost_accumulate_as_the_run_goes() {
        let mut t = AgentTree::default();
        t.apply(&started("a", "r1", None, "anthropic/claude-haiku-4-5"), T0);
        t.apply(&tool("r1", "c1", "read_file", json!({"path": "src/lib.rs"})), T0 + 10);
        assert_eq!(t.node("r1").unwrap().activity, "read_file src/lib.rs");
        t.apply(&model_step("r1", 1_000, 200), T0 + 20);
        t.apply(&tool("r1", "c2", "read_file", json!({"path": "b"})), T0 + 30);
        t.apply(&tool("r1", "c3", "http_get", json!({"url": "https://x"})), T0 + 40);
        let n = t.node("r1").unwrap();
        assert_eq!(n.tools, ["read_file", "http_get"], "each tool once, in order of first use");
        assert_eq!(n.tokens, 1_200);
        // haiku is $1 per million tokens in and $5 out: 1000 in is 1000 micro-USD, 200 out is 1000
        assert_eq!(n.cost_micros, Some(2_000));
    }

    #[test]
    fn what_a_model_costs_is_free_here_priced_when_known_and_unknown_otherwise() {
        assert_eq!(price("local/system", 1000, 1000), Some(0));
        assert_eq!(price("mock/mock", 1000, 1000), Some(0));
        assert!(price("anthropic/claude-sonnet-5-5", 1_000_000, 0).unwrap() > 0);
        assert_eq!(price("openai/some-unheard-of-model", 1000, 1000), None);
    }

    #[test]
    fn a_finished_run_says_how_it_ended() {
        let mut t = AgentTree::default();
        for (run, status, want) in [
            ("r1", "ok", Status::Done),
            ("r2", "failed", Status::Failed),
            ("r3", "over_budget", Status::Stopped),
            ("r4", "dropped", Status::Stopped),
        ] {
            t.apply(&started("a", run, None, "x/y"), T0);
            t.apply(&finished(run, status, T0 + 5_000), T0 + 5_000);
            let n = t.node(run).unwrap();
            assert_eq!(n.status, want, "{status}");
            assert_eq!(n.finished_ms, Some(T0 + 5_000));
            assert_eq!(n.result.as_deref(), Some("the answer"));
            assert_eq!(n.elapsed_ms(T0 + 99_000), 5_000, "a finished run stops counting");
        }
    }

    #[test]
    fn an_agent_waiting_for_a_person_needs_you_until_it_is_answered() {
        let mut t = AgentTree::default();
        t.apply(&started("writer", "r1", None, "x/y"), T0);
        t.apply(
            &Event::ApprovalRequested {
                id: 7,
                agent: "writer".into(),
                tool: "write_file".into(),
                args: json!({"path": "a"}),
                chain: vec![],
            },
            T0,
        );
        assert_eq!(t.node("r1").unwrap().status, Status::NeedsYou);
        assert_eq!(t.summary(T0).waiting.len(), 1);
        assert_eq!(t.summary(T0).waiting[0].pending.id, 7);
        t.apply(&Event::ApprovalResolved { id: 7, approved: true }, T0);
        assert_eq!(t.node("r1").unwrap().status, Status::Running);
        assert!(t.summary(T0).waiting.is_empty());
    }

    fn session(id: &str, run: &str, phase: Phase, cost: u64) -> SessionView {
        SessionView {
            id: id.into(),
            agent: "lead".into(),
            run_id: run.into(),
            call_span: None,
            trace_id: "t".into(),
            task: "make the test pass".into(),
            model: "anthropic/claude-sonnet-5-5".into(),
            workspace: "/w".into(),
            phase,
            cost_micros: cost,
            tokens_in: 900,
            tokens_out: 100,
            tools: vec!["read_file".into()],
            calls: 1,
            activity: "read_file a".into(),
            started_ms: T0,
            finished_ms: None,
            result: None,
            pending: vec![],
        }
    }

    #[test]
    fn a_coding_session_hangs_under_the_run_that_started_it() {
        let mut t = AgentTree::default();
        t.apply(&started("lead", "r1", None, "x/y"), T0);
        t.apply(&Event::Session(Box::new(session("s1", "r1", Phase::Running, 1_500))), T0 + 50);
        let kids = t.children_of("r1");
        assert_eq!(kids.len(), 1);
        let s = kids[0];
        assert_eq!((s.kind, s.status), (Kind::Session, Status::Running));
        assert_eq!(s.cost_micros, Some(1_500));
        assert_eq!(s.tokens, 1_000);
        assert_eq!(s.tools, ["read_file"]);
        let mut v = session("s1", "r1", Phase::Done, 250_000);
        v.finished_ms = Some(T0 + 9_000);
        v.result = Some("fixed".into());
        t.apply(&Event::Session(Box::new(v)), T0 + 9_000);
        assert_eq!(t.len(), 2);
        let s = t.node("session:s1").unwrap();
        assert_eq!((s.status, s.cost_micros), (Status::Done, Some(250_000)));
        assert_eq!(s.elapsed_ms(T0 + 99_000), 9_000);
    }

    #[test]
    fn a_session_waiting_on_a_call_shows_it_and_is_answered_through_the_same_id() {
        let mut t = AgentTree::default();
        t.apply(&started("lead", "r1", None, "x/y"), T0);
        let mut v = session("s1", "r1", Phase::NeedsYou, 10);
        v.pending = vec![agent_wire::Pending {
            id: 3,
            agent: "lead".into(),
            tool: "session:write_file".into(),
            args: json!({"path": "fix.txt"}),
            chain: vec!["session s1".into()],
        }];
        t.apply(&Event::Session(Box::new(v)), T0);
        let w = t.summary(T0).waiting;
        assert_eq!(w.len(), 1);
        assert_eq!(
            (w[0].pending.id, w[0].pending.tool.as_str(), w[0].pending.session),
            (3, "write_file", true)
        );
        assert_eq!(w[0].agent, "Coder");
        assert_eq!(w[0].via, ["lead"], "whom it is for");
        // the runtime says it too, as it says every approval: it must not be counted twice
        t.apply(
            &Event::ApprovalRequested {
                id: 3,
                agent: "lead".into(),
                tool: "session:write_file".into(),
                args: json!({"path": "fix.txt"}),
                chain: vec!["session s1".into()],
            },
            T0,
        );
        assert_eq!(t.summary(T0).waiting.len(), 1);
        t.apply(&Event::ApprovalResolved { id: 3, approved: true }, T0);
        assert!(t.summary(T0).waiting.is_empty());
    }

    #[test]
    fn the_header_counts_agents_running_waiting_and_what_it_all_cost() {
        let mut t = AgentTree::default();
        t.apply(&started("lead", "r1", None, "local/system"), T0);
        t.apply(&Event::Session(Box::new(session("s1", "r1", Phase::Running, 400_000))), T0);
        t.apply(&Event::Session(Box::new(session("s2", "r1", Phase::Running, 600_000))), T0);
        let mut done = session("s2", "r1", Phase::Done, 600_000);
        done.finished_ms = Some(T0 + 1);
        t.apply(&Event::Session(Box::new(done)), T0 + 1);
        let s = t.summary(T0 + 2);
        assert_eq!((s.agents, s.running), (3, 2));
        assert_eq!(s.cost_micros, 1_000_000);
    }

    #[test]
    fn finished_trees_leave_the_board_lone_ones_sooner() {
        let mut t = AgentTree::default();
        t.apply(&started("chat", "lone", None, "x/y"), T0);
        t.apply(&finished("lone", "ok", T0 + 1_000), T0 + 1_000);
        t.apply(&started("lead", "big", None, "x/y"), T0);
        t.apply(&Event::Session(Box::new(session("s1", "big", Phase::Done, 1))), T0);
        let mut v = session("s1", "big", Phase::Done, 1);
        v.finished_ms = Some(T0 + 2_000);
        t.apply(&Event::Session(Box::new(v)), T0 + 2_000);
        t.apply(&finished("big", "ok", T0 + 2_000), T0 + 2_000);
        let ids = |now| t.visible_roots(now).iter().map(|n| n.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(T0 + 3_000), ["big", "lone"].map(String::from), "newest first");
        assert_eq!(ids(T0 + 1_000 + KEEP_LONE_MS + 1), ["big"].map(String::from));
        assert!(ids(T0 + 2_000 + KEEP_TREE_MS + 1).is_empty());
    }

    #[test]
    fn a_tree_with_something_still_going_never_leaves() {
        let mut t = AgentTree::default();
        t.apply(&started("lead", "r1", None, "x/y"), T0);
        assert_eq!(t.visible_roots(T0 + 100 * KEEP_TREE_MS).len(), 1);
    }

    #[test]
    fn a_session_row_says_how_the_whole_goal_stands() {
        let mut t = AgentTree::default();
        t.apply(&started("lead", "r1", None, "x/y"), T0);
        t.apply(&Event::Session(Box::new(session("s1", "r1", Phase::NeedsYou, 5_000))), T0 + 10);
        let mut v = session("s1", "r1", Phase::NeedsYou, 5_000);
        v.pending = vec![agent_wire::Pending {
            id: 4,
            agent: "lead".into(),
            tool: "session:run".into(),
            args: json!({}),
            chain: vec!["session s1".into()],
        }];
        t.apply(&Event::Session(Box::new(v)), T0 + 20);
        let rows = t.sessions(T0 + 30);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(
            (r.root.as_str(), r.lead.as_str(), r.title.as_str()),
            ("r1", "lead", "lead's task")
        );
        assert_eq!((r.status, r.agents, r.waiting, r.cost_micros), (Status::NeedsYou, 2, 1, 5_000));
        assert_eq!(r.finished_ms, None);

        t.apply(&finished("r1", "ok", T0 + 40), T0 + 40);
        assert_eq!(t.sessions(T0 + 50)[0].status, Status::NeedsYou);
        let mut done = session("s1", "r1", Phase::Done, 9_000);
        done.finished_ms = Some(T0 + 90);
        t.apply(&Event::Session(Box::new(done)), T0 + 90);
        let r = &t.sessions(T0 + 100)[0];
        assert_eq!((r.status, r.finished_ms), (Status::Done, Some(T0 + 90)));
    }

    #[test]
    fn the_header_of_one_session_counts_only_that_session() {
        let mut t = AgentTree::default();
        t.apply(&started("a", "ra", None, "x/y"), T0);
        t.apply(&started("b", "rb", None, "x/y"), T0 + 1);
        t.apply(&Event::Session(Box::new(session("s1", "ra", Phase::Running, 700))), T0 + 2);
        assert_eq!(t.summary_of("ra").agents, 2);
        assert_eq!(t.summary_of("rb").agents, 1);
        assert_eq!(t.summary_of("ra").cost_micros, 700);
    }

    #[test]
    fn what_ran_before_comes_back_with_its_parents() {
        let record = |id: &str,
                      agent: &str,
                      started: u64,
                      parent: Option<&str>,
                      tools: &[(&str, &str)]| {
            serde_json::from_value::<RunRecord>(json!({
                "id": id, "agent": agent, "trigger": "http", "input": format!("{agent} did it"),
                "started": 0, "finished": 0, "status": "ok", "answer": "fine",
                "steps": tools.iter().map(|(n, sp)| json!({
                    "kind": "tool", "name": n, "args": {}, "result": "", "error": false,
                    "approved": null, "span_id": sp, "t0": 0, "t1": 0})).collect::<Vec<_>>(),
                "tokens_in": 10, "tokens_out": 5, "started_ms": started, "finished_ms": started + 500,
                "model": "local/system", "parent_span_id": parent,
            }))
            .unwrap()
        };
        let runs = vec![
            record("child", "helper", T0 + 100, Some("call-1"), &[]),
            record(
                "lead",
                "lead",
                T0,
                None,
                &[("agent:helper", "call-1"), ("read_file", "call-2")],
            ),
        ];
        let mut t = AgentTree::default();
        t.seed(&runs);
        assert_eq!(t.roots().len(), 1);
        assert_eq!(t.children_of("lead")[0].id, "child");
        let lead = t.node("lead").unwrap();
        assert_eq!(
            (lead.status, lead.tools.clone(), lead.finished_ms),
            (Status::Done, vec!["read_file".to_string()], Some(T0 + 500))
        );
        assert_eq!(lead.result.as_deref(), Some("fine"));
        t.seed(&runs);
        assert_eq!(t.len(), 2);
    }
}
