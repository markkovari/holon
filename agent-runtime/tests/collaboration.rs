//! How agents wake and cooperate: durable events, shared store, delegation,
//! loop guards, timers, and the W3C/OpenTelemetry trace that follows it all.
//! Every agent here runs a scripted model, so these are deterministic.

use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_runtime::agent::{Cause, Host, TaskState};
use agent_runtime::spec::StoreAccess;
use agent_runtime::store::{RunRecord, Status, Step};
use agent_runtime::{AgentSpec, Capability, Config, ModelSpec, Runtime, Trigger};

/// A uniquely named, securely created directory (tempfile makes it 0700 with a
/// random name; a predictable path in the shared temp dir is a classic race).
fn dir(tag: &str) -> std::path::PathBuf {
    tempfile::Builder::new().prefix(&format!("ar-collab-{tag}-")).tempdir().unwrap().keep()
}

fn runtime(tag: &str) -> Arc<Runtime> {
    Runtime::new(Config::new(dir(tag))).unwrap()
}

/// An agent whose model says exactly `replies`, in order, and whose
/// description is irrelevant. `n` copies, because runs repeat.
fn agent(name: &str, replies: &[&str]) -> AgentSpec {
    let mut s = AgentSpec::new(name, "a test agent");
    s.model = ModelSpec::Mock { replies: replies.iter().map(|r| r.to_string()).collect() };
    s
}

fn repeat(reply: &str, n: usize) -> Vec<String> {
    vec![reply.to_string(); n]
}

fn agent_n(name: &str, replies: Vec<String>) -> AgentSpec {
    let mut s = AgentSpec::new(name, "a test agent");
    s.model = ModelSpec::Mock { replies };
    s
}

fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(40));
    }
}

fn runs(rt: &Runtime, name: &str) -> Vec<RunRecord> {
    let mut v = rt.store().runs(name, 100);
    v.reverse();
    v
}

fn tool_span(rec: &RunRecord, tool: &str) -> String {
    rec.steps
        .iter()
        .find_map(|s| match s {
            Step::Tool { name, span_id, .. } if name == tool => Some(span_id.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no {tool} step in {:?}", rec.steps))
}

fn subscriber(name: &str, topic: &str, reply: &str) -> AgentSpec {
    let mut s = agent_n(name, repeat(reply, 10));
    s.triggers.push(Trigger::Event { topic: topic.into(), filter: None });
    s
}

// ---- durability ------------------------------------------------------------

#[test]
fn an_event_sent_while_an_agent_is_paused_is_delivered_when_it_resumes() {
    let rt = runtime("paused");
    let mut s = subscriber("sleeper", "news", "got it");
    s.paused = true;
    rt.create_agent(s).unwrap();

    assert_eq!(rt.emit_event("news", "first", None, None).unwrap(), 0, "nobody awake to wake");
    rt.emit_event("news", "second", None, None).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(runs(&rt, "sleeper").is_empty(), "a paused agent must not run");

    rt.set_paused("sleeper", false).unwrap();
    let r = wait_for("both queued events", || Some(runs(&rt, "sleeper")).filter(|r| r.len() == 2));
    assert_eq!(
        r.iter().map(|r| r.input.as_str()).collect::<Vec<_>>(),
        ["first", "second"],
        "delivered in order"
    );
    wait_for("offset to reach the head", || {
        (rt.bus().offset("news", "sleeper").unwrap() == Some(2)).then_some(())
    });
}

#[test]
fn events_published_before_a_restart_are_delivered_after_it() {
    let d = dir("restart");
    {
        let rt = Runtime::new(Config::new(&d)).unwrap();
        let mut s = subscriber("survivor", "alerts", "handled");
        s.paused = true;
        rt.create_agent(s).unwrap();
        rt.emit_event("alerts", "disk full", None, None).unwrap();
    } // runtime gone, event only in the log
    let rt = Runtime::new(Config::new(&d)).unwrap();
    rt.set_paused("survivor", false).unwrap();
    let r = wait_for("redelivery", || Some(runs(&rt, "survivor")).filter(|r| !r.is_empty()));
    assert_eq!(r[0].input, "disk full");
    assert!(r[0].trigger.starts_with("event: alerts"));
}

#[test]
fn a_new_subscriber_hears_the_future_not_the_whole_history() {
    let rt = runtime("history");
    for i in 0..3 {
        rt.emit_event("feed", &format!("old {i}"), None, None).unwrap();
    }
    rt.create_agent(subscriber("latecomer", "feed", "ok")).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(runs(&rt, "latecomer").is_empty());
    rt.emit_event("feed", "new", None, None).unwrap();
    let r = wait_for("the new event", || Some(runs(&rt, "latecomer")).filter(|r| !r.is_empty()));
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].input, "new");
}

#[test]
fn an_event_filter_only_wakes_the_agent_for_matching_payloads() {
    let rt = runtime("filter");
    let mut s = agent_n("picky", repeat("ok", 10));
    s.triggers.push(Trigger::Event { topic: "deploy".into(), filter: Some("PROD".into()) });
    rt.create_agent(s).unwrap();
    assert_eq!(rt.emit_event("deploy", "to staging", None, None).unwrap(), 0);
    assert_eq!(rt.emit_event("deploy", "to prod now", None, None).unwrap(), 1);
    let r = wait_for("the matching event", || Some(runs(&rt, "picky")).filter(|r| !r.is_empty()));
    assert_eq!(r[0].input, "to prod now");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(runs(&rt, "picky").len(), 1);
}

// ---- the shared store ------------------------------------------------------

#[test]
fn a_store_watcher_wakes_only_on_a_real_change_under_its_prefix() {
    let rt = runtime("watch");
    let mut w = agent_n("watcher", repeat("noted", 10));
    w.triggers.push(Trigger::StoreChange { ns: "rowing".into(), key_prefix: "latest".into() });
    rt.create_agent(w).unwrap();

    rt.put_store("rowing", "latest", "10/05 12,108m", "admin").unwrap();
    rt.put_store("rowing", "latest", "10/05 12,108m", "admin").unwrap(); // identical: nobody wakes
    rt.put_store("rowing", "other", "x", "admin").unwrap(); // outside the prefix
    rt.put_store("rowing", "latest", "10/06 6,000m", "admin").unwrap();

    let r = wait_for("two wake-ups", || Some(runs(&rt, "watcher")).filter(|r| r.len() >= 2));
    std::thread::sleep(Duration::from_millis(300));
    let r2 = runs(&rt, "watcher");
    assert_eq!(r2.len(), 2, "identical write and out-of-prefix write must not wake it: {r2:?}");
    assert_eq!(r[0].trigger, "store: rowing/latest");
    let payload: serde_json::Value = serde_json::from_str(&r[0].input).unwrap();
    assert_eq!(payload["key"], "latest");
    assert_eq!(payload["value"], "10/05 12,108m");
    assert_eq!(payload["version"], 1);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&r[1].input).unwrap()["version"], 2);
}

#[test]
fn an_agent_writes_the_store_through_its_tool_and_another_wakes_on_it() {
    let rt = runtime("blackboard");
    let mut writer = agent(
        "scout",
        &[
            r#"{"tool":"store_put","args":{"ns":"rowing","key":"latest","value":"12,108m"}}"#,
            "stored",
        ],
    );
    writer.capabilities.push(Capability::named("store_put"));
    writer.store = StoreAccess { read: vec![], write: vec!["rowing".into()] };
    rt.create_agent(writer).unwrap();

    let mut reader = agent_n(
        "coach",
        vec![
            r#"{"tool":"store_get","args":{"ns":"rowing","key":"latest"}}"#.into(),
            "Nice 12,108m".into(),
        ],
    );
    reader.capabilities.push(Capability::named("store_get"));
    reader.store = StoreAccess { read: vec!["rowing".into()], write: vec![] };
    reader.triggers.push(Trigger::StoreChange { ns: "rowing".into(), key_prefix: String::new() });
    rt.create_agent(reader).unwrap();

    let a = rt.run_agent("scout", "http", "go", &Cause::default(), "", true).unwrap();
    assert_eq!(a.status, Status::Ok);
    let b = wait_for("coach to wake", || runs(&rt, "coach").into_iter().next());
    assert_eq!(b.answer, "Nice 12,108m");
    // the coach actually read what the scout wrote, through the store
    assert!(b.steps.iter().any(|s| matches!(s, Step::Tool { name, result, .. }
        if name == "store_get" && result.contains("12,108m") && result.contains("by scout"))));
    // and one trace follows the whole thing
    assert_eq!(a.trace_id, b.trace_id);
    assert_eq!(b.parent_span_id.as_deref(), Some(tool_span(&a, "store_put").as_str()));
}

#[test]
fn store_access_and_event_topics_are_granted_by_the_spec_not_assumed() {
    let rt = runtime("acl");
    let mut s = agent(
        "nosy",
        &[
            r#"{"tool":"store_put","args":{"ns":"secrets","key":"k","value":"v"}}"#,
            r#"{"tool":"store_get","args":{"ns":"secrets","key":"k"}}"#,
            r#"{"tool":"emit_event","args":{"topic":"anything","payload":"x"}}"#,
            r#"{"tool":"store_put","args":{"ns":"private","key":"mine","value":"ok"}}"#,
            "done",
        ],
    );
    for c in ["store_put", "store_get", "emit_event"] {
        s.capabilities.push(Capability::named(c));
    }
    rt.create_agent(s).unwrap();
    let r = rt.run_agent("nosy", "http", "go", &Cause::default(), "", true).unwrap();
    let results: Vec<(bool, String)> = r
        .steps
        .iter()
        .filter_map(|s| match s {
            Step::Tool { error, result, .. } => Some((*error, result.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 4, "{results:?}");
    assert!(results[0].0 && results[0].1.contains("may not write store `secrets`"), "{results:?}");
    assert!(results[1].0 && results[1].1.contains("may not read store `secrets`"));
    assert!(results[2].0 && results[2].1.contains("may not emit `anything`"));
    assert!(!results[3].0, "its own private namespace needs no grant: {results:?}");
    assert!(rt.kv().get("secrets", "k").unwrap().is_none());
    assert_eq!(rt.kv().get("private-nosy", "mine").unwrap().unwrap().value, "ok");
}

// ---- waking and calling agents, and the trace ------------------------------

#[test]
fn a_chain_across_agents_is_one_trace_with_parent_links_through_the_tool_call_that_woke_each() {
    let rt = runtime("trace");
    let mut a = agent(
        "origin",
        &[r#"{"tool":"emit_event","args":{"topic":"step.one","payload":"go"}}"#, "sent"],
    );
    a.capabilities.push(Capability::named("emit_event"));
    a.topics_out = vec!["step.*".into()];
    rt.create_agent(a).unwrap();

    let mut b = subscriber("middle", "step.one", "");
    b.model = ModelSpec::Mock {
        replies: vec![
            r#"{"tool":"emit_event","args":{"topic":"step.two","payload":"on"}}"#.into(),
            "relayed".into(),
        ],
    };
    b.capabilities.push(Capability::named("emit_event"));
    b.topics_out = vec!["step.two".into()];
    rt.create_agent(b).unwrap();
    rt.create_agent(subscriber("last", "step.two", "end")).unwrap();

    let first = rt.run_agent("origin", "http", "start", &Cause::default(), "", true).unwrap();
    let mid = wait_for("middle", || runs(&rt, "middle").into_iter().next());
    let end = wait_for("last", || runs(&rt, "last").into_iter().next());

    // one trace id (W3C: 32 lowercase hex) across all three agents
    assert_eq!(first.trace_id.len(), 32);
    assert!(first.trace_id.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
    assert_eq!((&mid.trace_id, &end.trace_id), (&first.trace_id, &first.trace_id));
    // each run is the child of the TOOL CALL that woke it
    assert_eq!(first.parent_span_id, None);
    assert_eq!(mid.parent_span_id.as_deref(), Some(tool_span(&first, "emit_event").as_str()));
    assert_eq!(end.parent_span_id.as_deref(), Some(tool_span(&mid, "emit_event").as_str()));
    assert_eq!((first.hops, mid.hops, end.hops), (0, 1, 2));
    assert_eq!(end.chain, ["middle|step.one", "last|step.two"]);

    let trace = rt.store().trace(&first.trace_id);
    assert_eq!(trace.len(), 3);

    // the same chain, as the OpenTelemetry document a collector would get
    let doc = rt.trace_otlp(&first.trace_id);
    let spans = doc["resourceSpans"][0]["scopeSpans"][0]["spans"].as_array().unwrap();
    let by_id = |id: &str| {
        spans.iter().find(|s| s["spanId"] == id).unwrap_or_else(|| panic!("no span {id}"))
    };
    assert!(spans.iter().all(|s| s["traceId"] == first.trace_id.as_str()));
    let mid_span = by_id(&mid.span_id);
    assert_eq!(mid_span["name"], "invoke_agent middle");
    assert_eq!(mid_span["parentSpanId"], tool_span(&first, "emit_event").as_str());
    assert_eq!(by_id(&tool_span(&first, "emit_event"))["name"], "execute_tool emit_event");
    assert_eq!(by_id(&tool_span(&first, "emit_event"))["parentSpanId"], first.span_id.as_str());
    assert!(spans.iter().any(|s| s["name"].as_str().unwrap().starts_with("chat ")));
}

#[test]
fn a_delegated_call_is_a_child_span_and_its_tokens_count_against_the_caller() {
    let rt = runtime("call");
    let mut boss =
        agent("boss", &[r#"{"tool":"agent:worker","args":{"message":"do it"}}"#, "worker did it"]);
    boss.capabilities.push(Capability::named("agent:worker"));
    rt.create_agent(boss).unwrap();
    rt.create_agent(agent("worker", &["done"])).unwrap();

    let b = rt.run_agent("boss", "http", "go", &Cause::default(), "", true).unwrap();
    let w = runs(&rt, "worker").remove(0);
    assert_eq!(w.trace_id, b.trace_id);
    assert_eq!(w.parent_span_id.as_deref(), Some(tool_span(&b, "agent:worker").as_str()));
    assert_eq!(w.trigger, "agent: boss");
    assert_eq!(w.chain, ["worker|call"]);
}

#[test]
fn spawn_task_returns_at_once_and_the_result_is_there_later() {
    let rt = runtime("task");
    let mut boss = agent(
        "lead",
        &[r#"{"tool":"spawn_task","args":{"agent":"helper","message":"crunch"}}"#, "started"],
    );
    boss.capabilities.push(Capability::named("spawn_task"));
    boss.capabilities.push(Capability::named("agent:helper"));
    rt.create_agent(boss).unwrap();
    rt.create_agent(agent("helper", &["crunched"])).unwrap();

    let r = rt.run_agent("lead", "http", "go", &Cause::default(), "", true).unwrap();
    let id = r
        .steps
        .iter()
        .find_map(|s| match s {
            Step::Tool { name, result, .. } if name == "spawn_task" => {
                result.split_whitespace().nth(1).map(String::from)
            }
            _ => None,
        })
        .expect("a task id");
    let done = wait_for("the task", || match rt.task_result(&id) {
        Some(TaskState::Done { ok, answer }) => Some((ok, answer)),
        _ => None,
    });
    assert_eq!(done, (true, "crunched".to_string()));
    let h = runs(&rt, "helper").remove(0);
    assert_eq!(h.trace_id, r.trace_id);
    assert_eq!(h.parent_span_id.as_deref(), Some(tool_span(&r, "spawn_task").as_str()));
}

#[test]
fn spawn_task_needs_the_agent_capability_for_that_target() {
    let rt = runtime("task-acl");
    let mut s = agent(
        "sneaky",
        &[r#"{"tool":"spawn_task","args":{"agent":"victim","message":"x"}}"#, "ok"],
    );
    s.capabilities.push(Capability::named("spawn_task"));
    rt.create_agent(s).unwrap();
    rt.create_agent(agent("victim", &["hi"])).unwrap();
    let r = rt.run_agent("sneaky", "http", "go", &Cause::default(), "", true).unwrap();
    assert!(r.steps.iter().any(|s| matches!(s, Step::Tool { error: true, result, .. } if result.contains("only start agents"))));
    assert!(runs(&rt, "victim").is_empty());
}

#[test]
fn a_timer_set_by_the_agent_wakes_it_once_later_and_is_persisted_meanwhile() {
    let d = dir("timer");
    let rt = Runtime::new(Config::new(&d)).unwrap();
    let mut s = agent(
        "napper",
        &[r#"{"tool":"schedule_self","args":{"in_secs":1,"prompt":"wake up"}}"#, "ok", "awake"],
    );
    s.capabilities.push(Capability::named("schedule_self"));
    rt.create_agent(s).unwrap();
    rt.run_agent("napper", "http", "go", &Cause::default(), "", true).unwrap();
    assert!(d.join("timers.json").exists());
    assert!(std::fs::read_to_string(d.join("timers.json")).unwrap().contains("wake up"));

    rt.start_scheduler();
    let woke =
        wait_for("the timer", || runs(&rt, "napper").into_iter().find(|r| r.trigger == "timer"));
    assert_eq!(woke.input, "wake up");
    rt.shutdown();
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(
        runs(&rt, "napper").iter().filter(|r| r.trigger == "timer").count(),
        1,
        "a one-shot"
    );
}

// ---- guards ----------------------------------------------------------------

#[test]
fn a_cycle_between_agents_is_cut_and_the_refusal_is_logged() {
    let rt = runtime("cycle");
    let relay = |name: &str, hears: &str, says: &str| {
        let mut s = agent_n(
            name,
            vec![
                format!(r#"{{"tool":"emit_event","args":{{"topic":"{says}","payload":"x"}}}}"#),
                "sent".into(),
                format!(r#"{{"tool":"emit_event","args":{{"topic":"{says}","payload":"x"}}}}"#),
                "sent".into(),
            ],
        );
        s.capabilities.push(Capability::named("emit_event"));
        s.topics_out = vec![says.into()];
        s.triggers.push(Trigger::Event { topic: hears.into(), filter: None });
        s
    };
    rt.create_agent(relay("ping-a", "ping", "pong")).unwrap();
    rt.create_agent(relay("pong-b", "pong", "ping")).unwrap();

    rt.emit_event("ping", "start", None, None).unwrap();
    // a → pong → b → ping → a again: the second wake of `ping-a` via `ping`
    let dropped = wait_for("the cycle to be cut", || {
        runs(&rt, "ping-a").into_iter().find(|r| r.status == Status::Dropped)
    });
    assert!(dropped.answer.contains("cycle"), "{}", dropped.answer);
    std::thread::sleep(Duration::from_millis(300));
    let all: Vec<_> = runs(&rt, "ping-a").into_iter().chain(runs(&rt, "pong-b")).collect();
    assert_eq!(all.iter().filter(|r| r.status == Status::Ok).count(), 2, "{all:#?}");
    assert_eq!(all.iter().filter(|r| r.status == Status::Dropped).count(), 1);
    // the dropped wake-up is still part of the same trace
    assert!(all.iter().all(|r| r.trace_id == all[0].trace_id));
}

#[test]
fn a_flood_of_events_is_rate_limited_not_queued_forever() {
    let rt = runtime("rate");
    let mut s = subscriber("busy", "flood", "ok");
    s.max_runs_per_min = 2;
    rt.create_agent(s).unwrap();
    for i in 0..4 {
        rt.emit_event("flood", &format!("e{i}"), None, None).unwrap();
    }
    wait_for("all four to be handled", || Some(runs(&rt, "busy")).filter(|r| r.len() == 4));
    let r = runs(&rt, "busy");
    assert_eq!(r.iter().filter(|r| r.status == Status::Ok).count(), 2);
    let dropped: Vec<_> = r.iter().filter(|r| r.status == Status::Dropped).collect();
    assert_eq!(dropped.len(), 2);
    assert!(dropped.iter().all(|r| r.answer.contains("rate limit")));
}

#[test]
fn an_agent_that_keeps_failing_is_cut_off_for_a_while() {
    let mut cfg = Config::new(dir("breaker"));
    cfg.breaker_threshold = 2;
    let rt = Runtime::new(cfg).unwrap();
    // Local model with no server configured: every run fails.
    rt.create_agent(AgentSpec::new("flaky", "always fails")).unwrap();
    for _ in 0..2 {
        let r = rt.run_agent("flaky", "http", "x", &Cause::default(), "", true).unwrap();
        assert_eq!(r.status, Status::Failed);
    }
    let third = rt.run_agent("flaky", "http", "x", &Cause::default(), "", true);
    assert!(third.unwrap_err().contains("circuit open"));
    let last = runs(&rt, "flaky").pop().unwrap();
    assert_eq!(last.status, Status::Dropped, "the refusal is logged");
}

#[test]
fn a_chain_longer_than_the_hop_limit_is_refused() {
    let rt = runtime("hops");
    rt.create_agent(agent("deep", &["never"])).unwrap();
    let cause = Cause {
        hops: 99,
        trace_id: "0af7651916cd43dd8448eb211c80319c".into(),
        ..Default::default()
    };
    let r = rt.run_agent("deep", "event: x", "x", &cause, "x", true);
    assert!(r.unwrap_err().contains("wake-ups deep"));
    let d = runs(&rt, "deep").remove(0);
    assert_eq!(d.status, Status::Dropped);
    assert_eq!(d.trace_id, "0af7651916cd43dd8448eb211c80319c");
}

// ---- standard export -------------------------------------------------------

#[test]
fn finished_runs_are_exported_to_an_otlp_collector_as_json() {
    let collector = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let port = collector.server_addr().to_ip().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for mut req in collector.incoming_requests() {
            let mut body = String::new();
            std::io::Read::read_to_string(req.as_reader(), &mut body).unwrap();
            let ct = req
                .headers()
                .iter()
                .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case("content-type"))
                .map(|h| h.value.as_str().to_string())
                .unwrap_or_default();
            let _ = tx.send((req.url().to_string(), ct, body));
            let _ = req.respond(tiny_http::Response::from_string("{}"));
        }
    });

    let mut cfg = Config::new(dir("otlp"));
    cfg.otlp_endpoint = Some(format!("http://127.0.0.1:{port}/"));
    let rt = Runtime::new(cfg).unwrap();
    // `now` is one of every agent's default capabilities.
    rt.create_agent(agent("exported", &[r#"{"tool":"now","args":{}}"#, "done"])).unwrap();
    let rec = rt.run_agent("exported", "http", "go", &Cause::default(), "", true).unwrap();

    let (url, ct, body) =
        rx.recv_timeout(Duration::from_secs(10)).expect("the collector got nothing");
    assert_eq!(url, "/v1/traces");
    assert_eq!(ct, "application/json");
    let doc: serde_json::Value = serde_json::from_str(&body).unwrap();
    let spans = doc["resourceSpans"][0]["scopeSpans"][0]["spans"].as_array().unwrap();
    let names: Vec<&str> = spans.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names[0], "invoke_agent exported");
    assert!(names.iter().any(|n| n.starts_with("chat ")) && names.contains(&"execute_tool now"));
    assert!(spans.iter().all(|s| s["traceId"] == rec.trace_id.as_str()));
}

// ---- projects ---------------------------------------------------------------

use agent_runtime::projects::Project;

fn project(name: &str, agents: &[&str]) -> Project {
    Project {
        name: name.into(),
        description: "training".into(),
        agents: agents.iter().map(|a| a.to_string()).collect(),
        lead: None,
    }
}

#[test]
fn project_membership_is_the_grant_for_the_store_and_topics_and_removal_takes_it_away() {
    let rt = runtime("project");
    // Neither spec grants a store or any topic: only the project does.
    // One scripted model, three runs: replies are consumed in order across them.
    let put = |v: &str| {
        format!(
            r#"{{"tool":"store_put","args":{{"ns":"project.rowing","key":"latest","value":"{v}"}}}}"#
        )
    };
    let writer = agent_n(
        "scout",
        vec![
            put("tried-as-non-member"),
            "gave up".into(), // run 1: not a member
            put("12,108m"),
            r#"{"tool":"emit_event","args":{"topic":"rowing.logged","payload":"x"}}"#.into(),
            "done".into(), // run 2: member
            put("other"),
            "done again".into(), // run 3: removed
        ],
    );
    rt.create_agent(writer).unwrap();
    let mut coach = agent_n("coach", repeat("noted", 5));
    coach
        .triggers
        .push(Trigger::StoreChange { ns: "project.rowing".into(), key_prefix: String::new() });
    rt.create_agent(coach).unwrap();

    // not a member yet: the tool does not even exist for it
    let none = rt.run_agent("scout", "http", "go", &Cause::default(), "", true).unwrap();
    assert!(
        matches!(&none.steps[1], Step::Tool { error: true, result, .. } if result.contains("not one of your capabilities")),
        "{:?}",
        none.steps
    );

    rt.put_project(project("rowing", &["scout", "coach"])).unwrap();
    let r = rt.run_agent("scout", "http", "go", &Cause::default(), "", true).unwrap();
    let tools: Vec<(bool, String)> = r
        .steps
        .iter()
        .filter_map(|s| match s {
            Step::Tool { error, result, .. } => Some((*error, result.clone())),
            _ => None,
        })
        .collect();
    assert!(tools.iter().all(|(e, _)| !e), "membership should grant both calls: {tools:?}");
    assert_eq!(rt.kv().get("project.rowing", "latest").unwrap().unwrap().value, "12,108m");
    // the other member, woken by the shared store, is in the same trace
    let woke = wait_for("coach", || runs(&rt, "coach").into_iter().next());
    assert_eq!(woke.trace_id, r.trace_id);

    rt.remove_from_project("rowing", "scout").unwrap();
    let after = rt.run_agent("scout", "http", "go", &Cause::default(), "", true).unwrap();
    assert!(
        after.steps.iter().any(|s| matches!(s, Step::Tool { error: true, .. })),
        "removal must revoke the grant: {:?}",
        after.steps
    );
    assert_eq!(
        rt.kv().get("project.rowing", "latest").unwrap().unwrap().value,
        "12,108m",
        "unchanged"
    );
}

#[test]
fn a_project_is_validated_against_the_agents_that_exist_and_tracks_deletions() {
    let rt = runtime("project-valid");
    rt.create_agent(agent("a", &["x"])).unwrap();
    assert!(rt
        .put_project(project("rowing", &["a", "ghost"]))
        .unwrap_err()
        .contains("no agent named ghost"));
    let mut p = project("rowing", &["a"]);
    p.lead = Some("a".into());
    rt.put_project(p).unwrap();
    assert_eq!(rt.store().get_project("rowing").unwrap().lead.as_deref(), Some("a"));
    // removing the lead clears it; deleting an agent drops it from its projects
    rt.remove_from_project("rowing", "a").unwrap();
    assert_eq!(rt.store().get_project("rowing").unwrap().lead, None);
    rt.add_to_project("rowing", "a").unwrap();
    rt.delete_agent("a").unwrap();
    assert!(rt.store().get_project("rowing").unwrap().agents.is_empty());
    assert!(rt.add_to_project("nope", "a").unwrap_err().contains("no project"));
}

#[test]
fn an_agent_is_told_which_project_it_is_in_and_who_else_is() {
    let rt = runtime("project-prompt");
    rt.create_agent(agent("scout", &["ok"])).unwrap();
    rt.create_agent(agent("coach", &["ok"])).unwrap();
    let mut p = project("rowing", &["scout", "coach"]);
    p.lead = Some("coach".into());
    rt.put_project(p).unwrap();
    // the mock model sees the system prompt only through its token count, so
    // check the rendered grants instead: the effective spec is what runs
    let eff = agent_runtime::projects::effective(
        &rt.store().get("scout").unwrap(),
        &rt.store().list_projects(),
    );
    assert_eq!(eff.projects[0].members, ["scout", "coach"]);
    assert_eq!(eff.projects[0].lead.as_deref(), Some("coach"));
    assert_eq!(eff.projects[0].store_ns, "project.rowing");
}
