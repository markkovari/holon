//! A scripted run for the board to watch: an orchestrating agent, the agents it calls, and the
//! coding sessions they start, with one session that stops to ask a person.
//!
//! It feeds the tree the same [`Event`]s the runtime does, so what is drawn here is what the real
//! console draws, and it is how the board is developed and photographed without a model, a daemon or
//! an API key. `agents-board` plays it.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use agent_runtime::agent::Event;
use agent_runtime::runtime::Pending as RtPending;
use agent_runtime::session::{Phase, SessionView};
use agent_runtime::store::Step;
use gpui::{App, AsyncApp, Entity};
use serde_json::{json, Value};

use super::tree::AgentTree;
use super::view::{now_ms, Actions, Scope};

/// One thing that happens `at` milliseconds into the story, built when it does.
struct Beat {
    at: u64,
    event: Box<dyn Fn(u64) -> Event>,
}

fn beat(at: u64, event: impl Fn(u64) -> Event + 'static) -> Beat {
    Beat { at, event: Box::new(event) }
}

fn started(
    at: u64,
    agent: &'static str,
    run: &'static str,
    parent: Option<&'static str>,
    model: &'static str,
    task: &'static str,
) -> Beat {
    beat(at, move |_| Event::RunStarted {
        agent: agent.into(),
        run_id: run.into(),
        trigger: "http".into(),
        input: task.into(),
        trace_id: "trace".into(),
        span_id: format!("span-{run}"),
        parent_span_id: parent.map(String::from),
        model: model.into(),
        chain: vec![],
    })
}

fn tool(
    at: u64,
    agent: &'static str,
    run: &'static str,
    span: &'static str,
    name: &'static str,
    args: Value,
) -> Beat {
    beat(at, move |_| Event::ToolStarted {
        agent: agent.into(),
        run_id: run.into(),
        span_id: span.into(),
        tool: name.into(),
        args: args.clone(),
    })
}

fn thought(at: u64, agent: &'static str, run: &'static str, tin: u64, tout: u64) -> Beat {
    beat(at, move |_| Event::StepDone {
        agent: agent.into(),
        run_id: run.into(),
        step: Step::Model {
            text: String::new(),
            span_id: String::new(),
            t0: 0,
            t1: 0,
            tokens_in: tin,
            tokens_out: tout,
        },
    })
}

#[allow(clippy::too_many_arguments)]
fn finished(
    at: u64,
    agent: &'static str,
    run: &'static str,
    model: &'static str,
    status: &'static str,
    answer: &'static str,
    tin: u64,
    tout: u64,
) -> Beat {
    beat(at, move |now| {
        let rec = serde_json::from_value(json!({
            "id": run, "agent": agent, "trigger": "http", "input": "", "started": 0,
            "finished": 0, "status": status, "answer": answer, "steps": [],
            "tokens_in": tin, "tokens_out": tout, "finished_ms": now, "model": model,
        }))
        .expect("a run record");
        Event::RunFinished(Box::new(rec))
    })
}

/// A session as of one moment. `started` is when the story began, so elapsed time is real.
#[allow(clippy::too_many_arguments)]
fn session(
    at: u64,
    id: &'static str,
    run: &'static str,
    model: &'static str,
    task: &'static str,
    phase: Phase,
    cost: u64,
    tools: &'static [&'static str],
    activity: &'static str,
    pending: Option<(u64, &'static str, Value)>,
    result: Option<&'static str>,
    begun: u64,
) -> Beat {
    beat(at, move |now| {
        let over = phase.is_over();
        Event::Session(Box::new(SessionView {
            id: id.into(),
            agent: "lead".into(),
            run_id: run.into(),
            call_span: None,
            trace_id: "trace".into(),
            task: task.into(),
            model: model.into(),
            workspace: "/work/holon".into(),
            phase,
            cost_micros: cost,
            tokens_in: cost / 3,
            tokens_out: cost / 20,
            tools: tools.iter().map(|t| t.to_string()).collect(),
            calls: tools.len() as u32 * 2,
            activity: activity.into(),
            started_ms: now.saturating_sub(begun),
            finished_ms: over.then_some(now),
            result: result.map(String::from),
            pending: pending
                .iter()
                .map(|(pid, tool, args)| RtPending {
                    id: *pid,
                    agent: "lead".into(),
                    tool: format!("session:{tool}"),
                    args: args.clone(),
                    chain: vec![format!("session {id}")],
                })
                .collect(),
        }))
    })
}

const OPUS: &str = "anthropic/claude-opus-5-5";
const SONNET: &str = "anthropic/claude-sonnet-5-5";
const GPT: &str = "openai/gpt-5";
const LOCAL: &str = "local/system";
const SHORT: [&str; 4] = ["list_dir", "read_file", "write_file", "run"];

/// The whole story, in the order things happen.
fn story() -> Vec<Beat> {
    let s1 = "Implement a token-bucket limiter in the ingress and wire the flag";
    let s2 = "Write tests for the limiter, run them, and fix what fails";
    vec![
        started(
            0,
            "lead",
            "r-lead",
            None,
            OPUS,
            "Add rate limiting to the gateway and ship it behind a flag",
        ),
        thought(500, "lead", "r-lead", 2_400, 180),
        tool(
            900,
            "lead",
            "r-lead",
            "c-plan",
            "agent:planner",
            json!({"message": "split this into coding tasks"}),
        ),
        started(
            1_300,
            "planner",
            "r-plan",
            Some("c-plan"),
            SONNET,
            "Split the rate-limiting work into coding tasks and pick the files",
        ),
        tool(1_900, "planner", "r-plan", "c-ls", "list_dir", json!({"path": "reconciler/src"})),
        thought(2_300, "planner", "r-plan", 3_100, 420),
        tool(
            2_600,
            "planner",
            "r-plan",
            "c-rd",
            "read_file",
            json!({"path": "reconciler/src/bin/ingress.rs"}),
        ),
        tool(
            3_000,
            "lead",
            "r-lead",
            "c-docs",
            "agent:writer",
            json!({"message": "document the flag"}),
        ),
        started(
            3_400,
            "writer",
            "r-docs",
            Some("c-docs"),
            LOCAL,
            "Write the docs for the new rate-limit flag",
        ),
        tool(3_700, "planner", "r-plan", "c-s1", "session_start", json!({"task": "limiter"})),
        session(
            3_900,
            "s1",
            "r-plan",
            SONNET,
            s1,
            Phase::Running,
            4_000,
            &["list_dir"],
            "list_dir reconciler/src",
            None,
            None,
            0,
        ),
        tool(4_300, "planner", "r-plan", "c-s2", "session_start", json!({"task": "tests"})),
        session(
            4_500,
            "s2",
            "r-plan",
            SONNET,
            s2,
            Phase::Running,
            2_000,
            &[],
            "thinking",
            None,
            None,
            0,
        ),
        thought(4_900, "writer", "r-docs", 900, 260),
        started(
            5_200,
            "reviewer",
            "r-rev",
            Some("c-plan"),
            GPT,
            "Review the limiter diff and the gate output once the sessions finish",
        ),
        session(
            6_000,
            "s1",
            "r-plan",
            SONNET,
            s1,
            Phase::Running,
            21_000,
            &["list_dir", "read_file"],
            "read_file reconciler/src/bin/ingress.rs",
            None,
            None,
            2_100,
        ),
        session(
            7_200,
            "s2",
            "r-plan",
            SONNET,
            s2,
            Phase::Running,
            12_000,
            &["read_file"],
            "read_file tests/e2e.rs",
            None,
            None,
            2_700,
        ),
        session(
            8_600,
            "s1",
            "r-plan",
            SONNET,
            s1,
            Phase::Running,
            64_000,
            &["list_dir", "read_file", "write_file"],
            "write_file src/limiter.rs",
            None,
            None,
            4_700,
        ),
        finished(
            9_000,
            "writer",
            "r-docs",
            LOCAL,
            "ok",
            "Documented --rate-limit in docs/SELFHOST.md",
            1_800,
            640,
        ),
        session(
            10_000,
            "s2",
            "r-plan",
            SONNET,
            s2,
            Phase::NeedsYou,
            31_000,
            &["read_file", "write_file"],
            "waiting to write_file tests/limiter.rs",
            Some((900, "write_file", json!({"path": "tests/limiter.rs", "content": "…"}))),
            None,
            5_500,
        ),
    ]
}

/// What happens once the person has said yes: the rest of the story.
fn after_approval() -> Vec<Beat> {
    let s1 = "Implement a token-bucket limiter in the ingress and wire the flag";
    let s2 = "Write tests for the limiter, run them, and fix what fails";
    vec![
        session(
            300,
            "s2",
            "r-plan",
            SONNET,
            s2,
            Phase::Running,
            33_000,
            &["read_file", "write_file"],
            "write_file tests/limiter.rs",
            None,
            None,
            6_000,
        ),
        session(
            1_800,
            "s1",
            "r-plan",
            SONNET,
            s1,
            Phase::Running,
            118_000,
            &SHORT,
            "run cargo test -p comp-ingress",
            None,
            None,
            8_000,
        ),
        session(
            3_200,
            "s2",
            "r-plan",
            SONNET,
            s2,
            Phase::Running,
            57_000,
            &["read_file", "write_file", "run"],
            "run cargo test --test limiter",
            None,
            None,
            8_600,
        ),
        session(
            5_000,
            "s1",
            "r-plan",
            SONNET,
            s1,
            Phase::Done,
            164_000,
            &SHORT,
            "",
            None,
            Some("Added the limiter and --rate-limit; the ingress suite passes."),
            9_300,
        ),
        session(
            6_200,
            "s2",
            "r-plan",
            SONNET,
            s2,
            Phase::Done,
            92_000,
            &["read_file", "write_file", "run"],
            "",
            None,
            Some("Eight tests, all green after one fix."),
            10_000,
        ),
        thought(6_600, "planner", "r-plan", 2_900, 310),
        finished(
            7_000,
            "planner",
            "r-plan",
            SONNET,
            "ok",
            "Both coding sessions finished and passed their gates.",
            9_800,
            1_200,
        ),
        tool(7_300, "reviewer", "r-rev", "c-rv", "read_file", json!({"path": "diff.patch"})),
        thought(8_200, "reviewer", "r-rev", 6_400, 900),
        finished(
            9_200,
            "reviewer",
            "r-rev",
            GPT,
            "ok",
            "Approved: the limiter is covered and the flag defaults off.",
            7_100,
            1_300,
        ),
        thought(9_800, "lead", "r-lead", 4_200, 520),
        finished(
            10_400,
            "lead",
            "r-lead",
            OPUS,
            "ok",
            "Rate limiting is in behind --rate-limit, tested and reviewed.",
            11_000,
            1_900,
        ),
    ]
}

/// What a person's answer does in the story: it ends the wait, and the board hears it as it would.
pub struct DemoActions {
    tree: Entity<AgentTree>,
    answered: Cell<Option<bool>>,
}

impl Actions for DemoActions {
    fn answer(&self, id: u64, approve: bool, _scope: Scope, cx: &mut App) {
        self.answered.set(Some(approve));
        self.tree.update(cx, |t, cx| {
            t.apply(&Event::ApprovalResolved { id, approved: approve }, now_ms());
            cx.notify();
        });
    }
}

/// Plays `beats` into `tree`, each at its own time (milliseconds from when this was called).
async fn run_beats(tree: &Entity<AgentTree>, beats: Vec<Beat>, cx: &mut AsyncApp) -> bool {
    let mut last = 0;
    for b in beats {
        cx.background_executor().timer(Duration::from_millis(b.at.saturating_sub(last))).await;
        last = last.max(b.at);
        let ev = (b.event)(now_ms());
        let alive = tree.update(cx, |t, cx| {
            t.apply(&ev, now_ms());
            cx.notify();
        });
        if alive.is_err() {
            return false;
        }
    }
    true
}

/// Plays the story into `tree`. If nobody answers the approval within `auto`, it is approved, so an
/// unattended run (a screenshot) still gets to the end.
pub fn play(tree: Entity<AgentTree>, auto: Option<Duration>, cx: &mut App) -> Rc<DemoActions> {
    let actions = Rc::new(DemoActions { tree: tree.clone(), answered: Cell::new(None) });
    let me = actions.clone();
    cx.spawn(async move |cx| {
        if !run_beats(&tree, story(), cx).await {
            return;
        }
        // the approval: asked of a person, or of the clock
        let step = Duration::from_millis(100);
        let mut left = auto;
        while me.answered.get().is_none() {
            cx.background_executor().timer(step).await;
            if let Some(l) = left.as_mut() {
                *l = l.saturating_sub(step);
                if l.is_zero() {
                    let me = me.clone();
                    if cx.update(|app| me.answer(900, true, Scope::Once, app)).is_err() {
                        return;
                    }
                }
            }
        }
        run_beats(&tree, after_approval(), cx).await;
    })
    .detach();
    actions
}
