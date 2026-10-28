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

use agent_runtime::projects::Project;

fn project(name: &str, agents: &[&str]) -> Project {
    Project {
        name: name.into(),
        description: "training".into(),
        agents: agents.iter().map(|a| a.to_string()).collect(),
        lead: None,
        ..Default::default()
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
        &[("coach".into(), "Coaches rowing. Brief.".into())],
    );
    // members may ask each other, described, without any hand-written grant
    let ask = eff.capabilities.iter().find(|c| c.name == "agent:coach").unwrap();
    assert_eq!(ask.description, "Ask coach. Coaches rowing.");
    assert!(!eff.has_capability("agent:scout"));
    assert_eq!(eff.projects[0].members, ["scout", "coach"]);
    assert_eq!(eff.projects[0].lead.as_deref(), Some("coach"));
    assert_eq!(eff.projects[0].store_ns, "project.rowing");
}

#[test]
fn an_agent_granted_the_tools_can_create_change_pause_and_delete_other_agents() {
    let rt = runtime("admin");
    let create = r#"{"tool":"agent_save","args":{"name":"news","description":"Summarise the news.","capabilities":["remember"]}}"#;
    let change = r#"{"tool":"agent_save","args":{"name":"news","description":"Summarise the news in one line."}}"#;
    let pause = r#"{"tool":"agent_pause","args":{"name":"news","paused":true}}"#;
    let delete = r#"{"tool":"agent_delete","args":{"name":"news"}}"#;
    let selfdel = r#"{"tool":"agent_delete","args":{"name":"holon"}}"#;
    let mut holon =
        agent("holon", &[create, "ok", change, "ok", pause, "ok", selfdel, "ok", delete, "ok"]);
    for t in ["agent_save", "agent_pause", "agent_delete", "agents_list"] {
        holon.capabilities.push(Capability::named(t));
    }
    holon.auto_approve = vec!["agent_save".into(), "agent_delete".into()];
    rt.create_agent(holon).unwrap();

    let ask = |rt: &Arc<Runtime>| {
        rt.run_agent("holon", "http", "go", &Cause::default(), "", true).unwrap()
    };
    ask(&rt);
    let n = rt.store().get("news").expect("created");
    assert!(n.has_capability("remember"));
    ask(&rt);
    let n = rt.store().get("news").unwrap();
    assert_eq!(n.description, "Summarise the news in one line.");
    assert!(n.has_capability("remember"), "a change keeps the fields it did not mention");
    ask(&rt);
    assert!(rt.store().get("news").unwrap().paused);
    let r = ask(&rt);
    assert!(
        r.steps.iter().any(
            |s| matches!(s, Step::Tool { error: true, result, .. } if result.contains("yourself"))
        ),
        "an agent cannot delete itself"
    );
    assert!(rt.store().get("holon").is_some());
    ask(&rt);
    assert!(rt.store().get("news").is_none(), "deleted");
}

#[test]
fn removing_a_schedule_keeps_the_agent_its_history_and_its_other_triggers() {
    let rt = runtime("unschedule");
    let mut news = agent("news", &["ok"]);
    news.triggers.push(Trigger::Schedule {
        cron: "@every 1h".into(),
        prompt: "p".into(),
        notify: false,
        project: None,
    });
    news.triggers.push(Trigger::Schedule {
        cron: "0 12 * * *".into(),
        prompt: "q".into(),
        notify: false,
        project: None,
    });
    news.triggers.push(Trigger::Event { topic: "deploy".into(), filter: None });
    rt.create_agent(news).unwrap();
    let wrong = r#"{"tool":"agent_remove_schedule","args":{"name":"news","schedule":"@daily"}}"#;
    let one = r#"{"tool":"agent_remove_schedule","args":{"name":"news","schedule":"@every 1h"}}"#;
    let rest = r#"{"tool":"agent_remove_schedule","args":{"name":"news"}}"#;
    let mut holon = agent("holon", &[wrong, "ok", one, "ok", rest, "ok"]);
    holon.capabilities.push(Capability::named("agent_remove_schedule"));
    holon.auto_approve = vec!["agent_remove_schedule".into()];
    rt.create_agent(holon).unwrap();
    let ask = || rt.run_agent("holon", "http", "go", &Cause::default(), "", true).unwrap();

    let r = ask();
    assert!(
        r.steps.iter().any(|s| matches!(s, Step::Tool { error: true, result, .. }
            if result.contains("`@every 1h`") && result.contains("`0 12 * * *`"))),
        "a schedule that is not there is refused and the real ones are listed"
    );
    assert_eq!(rt.store().get("news").unwrap().triggers.len(), 3, "nothing was removed");

    ask();
    let left = rt.store().get("news").unwrap().triggers;
    assert_eq!(left.len(), 2, "one named schedule goes");
    assert!(left
        .iter()
        .any(|t| matches!(t, Trigger::Schedule { cron, .. } if cron == "0 12 * * *")));

    ask();
    let left = rt.store().get("news").unwrap().triggers;
    assert!(matches!(left.as_slice(), [Trigger::Event { .. }]), "all schedules go, events stay");
}

#[test]
fn a_deleted_agent_waits_in_the_trash_and_comes_back_whole() {
    let rt = runtime("trash");
    let mut news = agent("news", &["one", "two"]);
    news.triggers.push(Trigger::Event { topic: "deploy".into(), filter: None });
    rt.create_agent(news).unwrap();
    rt.put_project(project("rowing", &["news"])).unwrap();
    rt.run_agent("news", "http", "hi", &Cause::default(), "", true).unwrap();
    rt.store().remember("news", "the gate rejects a bare unwrap", 1).unwrap();
    std::fs::write(rt.store().workspace("news").unwrap().join("notes.txt"), "keep me").unwrap();

    rt.delete_agent("news").unwrap();
    assert!(rt.store().get("news").is_none(), "out of service");
    assert!(rt.store().runs("news", 10).is_empty() && rt.store().memories("news").is_empty());
    assert!(rt.store().get_project("rowing").unwrap().agents.is_empty(), "out of its projects");
    let trash = rt.store().trash();
    assert_eq!(trash.len(), 1);
    assert_eq!(
        (trash[0].name.as_str(), trash[0].runs, trash[0].projects.clone()),
        ("news", 1, vec!["rowing".to_string()])
    );

    // a name that is in use is never overwritten by a restore
    rt.create_agent(agent("news", &["x"])).unwrap();
    assert!(rt.restore_agent("news").unwrap_err().contains("exists"));
    rt.delete_agent("news").unwrap();
    assert_eq!(rt.store().trash().len(), 2);
    // the newest entry is the stand-in made above; the original is the older one
    rt.store().purge(&rt.store().trash().first().unwrap().id.clone()).unwrap();

    let back = rt.restore_agent("news").unwrap();
    assert_eq!(back.name, "news");
    assert!(rt.store().trash().is_empty(), "nothing left to restore");
    let n = rt.store().get("news").unwrap();
    assert!(matches!(n.triggers.as_slice(), [Trigger::Event { .. }]));
    assert_eq!(rt.store().runs("news", 10).len(), 1, "its run history");
    assert_eq!(rt.store().memories("news").len(), 1, "its memory");
    assert_eq!(
        std::fs::read_to_string(rt.store().workspace("news").unwrap().join("notes.txt")).unwrap(),
        "keep me",
        "its workspace"
    );
    assert_eq!(rt.store().get_project("rowing").unwrap().agents, ["news"], "its project");
    assert!(rt.restore_agent("news").unwrap_err().contains("trash is empty"));
}

#[test]
fn the_trash_is_emptied_after_thirty_days_and_a_purge_empties_it_early() {
    let rt = runtime("sweep");
    rt.create_agent(agent("old", &["x"])).unwrap();
    rt.create_agent(agent("young", &["x"])).unwrap();
    rt.create_agent(agent("gone", &["x"])).unwrap();
    for n in ["old", "young", "gone"] {
        rt.delete_agent(n).unwrap();
    }
    let at = |n: &str| rt.store().trash().into_iter().find(|t| t.name == n).unwrap().deleted_at;
    let day = 86_400;
    assert_eq!(rt.store().sweep_trash(at("old") + 30 * day - 1), 0, "not yet");
    rt.store().purge("gone").unwrap();
    assert_eq!(rt.store().sweep_trash(at("old") + 30 * day), 2, "the rest, at thirty days");
    assert!(rt.store().trash().is_empty());
}

fn sched(cron: &str, project: Option<&str>) -> Trigger {
    Trigger::Schedule {
        cron: cron.into(),
        prompt: "report".into(),
        notify: true,
        project: project.map(String::from),
    }
}

#[test]
fn a_schedule_for_a_project_wakes_a_member_for_it_and_refuses_a_stranger() {
    let rt = runtime("sched-project");
    let mut member = agent_n("member", repeat("done", 5));
    member.triggers.push(sched("@every 1s", Some("rowing")));
    let mut stranger = agent_n("stranger", repeat("done", 5));
    stranger.triggers.push(sched("@every 1s", Some("rowing")));
    let mut plain = agent_n("plain", repeat("done", 5));
    plain.triggers.push(sched("@every 1s", None));
    for a in [member, stranger, plain] {
        rt.create_agent(a).unwrap();
    }
    rt.put_project(project("rowing", &["member"])).unwrap();

    rt.tick(agent_runtime::runtime::unix_now() + 10);
    let m = wait_for("the member's run", || runs(&rt, "member").into_iter().next());
    assert_eq!(
        (m.status, m.project.as_str(), m.trigger.as_str()),
        (Status::Ok, "rowing", "schedule: @every 1s")
    );
    let st = wait_for("the stranger's refusal", || runs(&rt, "stranger").into_iter().next());
    assert_eq!(st.status, Status::Dropped);
    assert!(st.answer.contains("not a member of the project rowing"), "{}", st.answer);
    assert_eq!(st.project, "rowing", "the refusal still says which project");
    let p = wait_for("the plain run", || runs(&rt, "plain").into_iter().next());
    assert_eq!((p.status, p.project.as_str()), (Status::Ok, ""), "no project: the agent's own");
}

#[test]
fn a_reminder_keeps_the_project_it_was_asked_for_in() {
    let rt = runtime("remind-project");
    let ask = r#"{"tool":"schedule_self","args":{"in_secs":1,"prompt":"stretch"}}"#;
    let mut coach = agent("coach", &[ask, "noted", "time to stretch", ask, "noted"]);
    coach.capabilities.push(Capability::named("schedule_self"));
    rt.create_agent(coach).unwrap();
    rt.put_project(project("rowing", &["coach"])).unwrap();

    // asked for inside the project's room...
    let in_project = Cause { project: Some("rowing".into()), ..Default::default() };
    rt.run_agent(
        "coach",
        "http: mark wrote in the project rowing",
        "remind me",
        &in_project,
        "",
        true,
    )
    .unwrap();
    rt.tick(agent_runtime::runtime::unix_now() + 10);
    let fired =
        wait_for("the reminder", || runs(&rt, "coach").into_iter().find(|r| r.trigger == "timer"));
    assert_eq!(fired.project, "rowing", "it comes back where it was asked for");

    // ...and asked for in the agent's own room: no project
    rt.run_agent("coach", "http", "remind me", &Cause::default(), "", true).unwrap();
    rt.tick(agent_runtime::runtime::unix_now() + 20);
    let own = wait_for("the second reminder", || {
        runs(&rt, "coach").into_iter().filter(|r| r.trigger == "timer").nth(1)
    });
    assert_eq!(own.project, "");
}

#[test]
fn agent_save_with_a_project_joins_it_and_says_so() {
    let rt = runtime("save-project");
    rt.put_project(project("rowing", &[])).unwrap();
    let make = r#"{"tool":"agent_save","args":{"name":"cox","description":"steers","schedule":"@daily","prompt":"report","project":"rowing","notify":true}}"#;
    let nowhere =
        r#"{"tool":"agent_save","args":{"name":"lost","description":"d","project":"nope"}}"#;
    let mut holon = agent("holon", &[nowhere, "ok", make, "ok"]);
    holon.capabilities.push(Capability::named("agent_save"));
    holon.auto_approve = vec!["agent_save".into()];
    rt.create_agent(holon).unwrap();
    let ask = || rt.run_agent("holon", "http", "go", &Cause::default(), "", true).unwrap();

    let r = ask();
    assert!(
        r.steps.iter().any(|s| matches!(s, Step::Tool { error: true, result, .. } if result.contains("no project named nope"))),
        "a project that does not exist is refused before anything is created"
    );
    assert!(rt.store().get("lost").is_none());

    ask();
    let cox = rt.store().get("cox").expect("created");
    assert_eq!(rt.store().get_project("rowing").unwrap().agents, ["cox"], "joined the project");
    assert!(matches!(
        cox.triggers.as_slice(),
        [Trigger::Schedule { cron, notify: true, project: Some(p), .. }] if cron == "@daily" && p == "rowing"
    ));
}

#[test]
fn an_agent_can_only_hand_on_api_keys_it_holds() {
    let rt = runtime("save-keys");
    let theirs =
        r#"{"tool":"agent_save","args":{"name":"thief","description":"d","api_keys":["bank"]}}"#;
    let mine =
        r#"{"tool":"agent_save","args":{"name":"helper","description":"d","api_keys":["gh"]}}"#;
    let mut holon = agent("holon", &[theirs, "ok", mine, "ok"]);
    holon.capabilities.push(Capability::named("agent_save"));
    holon.auto_approve = vec!["agent_save".into()];
    holon.api_keys = vec!["gh".into()];
    rt.create_agent(holon).unwrap();
    let ask = || rt.run_agent("holon", "http", "go", &Cause::default(), "", true).unwrap();

    let r = ask();
    assert!(r.steps.iter().any(|s| matches!(s, Step::Tool { error: true, result, .. } if result.contains("you do not hold it"))));
    assert!(rt.store().get("thief").is_none());
    ask();
    assert_eq!(rt.store().get("helper").unwrap().api_keys, ["gh"]);
}

#[test]
fn removing_a_schedule_can_be_narrowed_to_one_project() {
    let rt = runtime("unschedule-project");
    let mut a = agent("news", &["ok"]);
    a.triggers.push(sched("@daily", Some("rowing")));
    a.triggers.push(sched("@daily", Some("nutrition")));
    rt.create_agent(a).unwrap();
    let one = r#"{"tool":"agent_remove_schedule","args":{"name":"news","schedule":"@daily","project":"rowing"}}"#;
    let mut holon = agent("holon", &[one, "ok"]);
    holon.capabilities.push(Capability::named("agent_remove_schedule"));
    holon.auto_approve = vec!["agent_remove_schedule".into()];
    rt.create_agent(holon).unwrap();
    rt.run_agent("holon", "http", "go", &Cause::default(), "", true).unwrap();
    let left = rt.store().get("news").unwrap().triggers;
    assert!(
        matches!(left.as_slice(), [Trigger::Schedule { project: Some(p), .. }] if p == "nutrition")
    );
}

#[test]
fn archiving_pauses_only_the_agents_that_lose_their_last_live_home_and_unarchiving_resumes_only_those(
) {
    let rt = runtime("archive");
    for n in ["chef", "coach", "rower"] {
        rt.create_agent(agent(n, &["x"])).unwrap();
    }
    rt.put_project(project("rowing", &["rower", "coach", "chef"])).unwrap();
    rt.put_project(project("side", &["coach"])).unwrap();
    rt.set_paused("rower", true).unwrap(); // the owner's own doing
    let paused = |n: &str| rt.store().get(n).unwrap().paused;

    // coach is still in a live project, rower was already paused: only chef is paused by this
    assert_eq!(rt.archive_project("rowing").unwrap(), ["chef"]);
    assert!(paused("chef") && !paused("coach") && paused("rower"));
    let p = rt.store().get_project("rowing").unwrap();
    assert!(p.archived);
    assert_eq!(p.paused_by_archive, ["chef"], "rower is not recorded: that pause was not ours");
    assert!(rt.archive_project("rowing").unwrap().is_empty(), "archiving twice changes nothing");

    // archive the other one: coach has no live home left now, so it is paused too
    assert_eq!(rt.archive_project("side").unwrap(), ["coach"]);
    assert!(paused("coach"));

    // bring rowing back: chef (rowing's record) and coach (side's record) resume, rower does not
    let mut resumed = rt.unarchive_project("rowing").unwrap();
    resumed.sort();
    assert_eq!(resumed, ["chef", "coach"]);
    assert!(
        !paused("chef") && !paused("coach") && paused("rower"),
        "rower stays as the owner left it"
    );
    assert!(rt.store().get_project("side").unwrap().paused_by_archive.is_empty());
    assert!(rt.store().get_project("side").unwrap().archived, "side is still shelved");
    assert!(rt.unarchive_project("rowing").unwrap().is_empty());
    assert!(rt.archive_project("nope").unwrap_err().contains("no project"));
}

#[test]
fn deleting_an_archived_project_does_not_leave_its_agents_paused() {
    let rt = runtime("archive-delete");
    rt.create_agent(agent("x", &["x"])).unwrap();
    rt.put_project(project("p", &["x"])).unwrap();
    rt.archive_project("p").unwrap();
    assert!(rt.store().get("x").unwrap().paused);
    rt.delete_project("p").unwrap();
    assert!(!rt.store().get("x").unwrap().paused);
    assert!(rt.store().get_project("p").is_none());
}

#[test]
fn granting_a_key_without_a_vault_fails_safely() {
    let rt = runtime("grant-key-novault");
    rt.create_agent(agent("bot", &["hi"])).unwrap();
    let err = rt.grant_key("k", "bot", true).unwrap_err();
    assert!(err.contains("no vault"));
    assert!(rt.grant_key("k", "bot", false).is_ok());
}

#[test]
fn an_agent_with_custom_pluggable_tools_reads_adds_and_never_deletes_without_approval() {
    use std::sync::Mutex;
    use agent_runtime::agent::ToolHandler;
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = seen.clone();

    struct MockCalendarHandler {
        log: Arc<Mutex<Vec<String>>>,
    }

    impl ToolHandler for MockCalendarHandler {
        fn can_handle(&self, name: &str) -> bool {
            matches!(name, "gcal_list" | "gcal_add" | "gcal_delete")
        }
        fn is_sensitive(&self, name: &str) -> bool {
            matches!(name, "gcal_add" | "gcal_delete")
        }
        fn execute(
            &self,
            _host: &dyn Host,
            name: &str,
            _args: &serde_json::Value,
            _cause: &Cause,
        ) -> Result<String, String> {
            self.log.lock().unwrap().push(name.to_string());
            match name {
                "gcal_list" => Ok("2026-10-15 19:00–20:00  Row  [Training]  (id e1)".into()),
                "gcal_add" => Ok("added: 2026-10-15 21:00–21:15  Stretch".into()),
                "gcal_delete" => Ok("deleted event e1".into()),
                _ => Err("unknown tool".into()),
            }
        }
    }

    let mut cfg = Config::new(dir("custom-tools"));
    cfg.approval_timeout = Duration::from_millis(200);
    let rt = Runtime::new(cfg).unwrap();
    rt.register_tool_handler(Arc::new(MockCalendarHandler { log }));

    let list = r#"{"tool":"gcal_list","args":{"from":"2026-10-15","calendar":"Training"}}"#;
    let add = r#"{"tool":"gcal_add","args":{"title":"Stretch","start":"2026-10-15T21:00","end":"2026-10-15T21:15","calendar":"Training"}}"#;
    let delete = r#"{"tool":"gcal_delete","args":{"calendar":"Training","id":"e1"}}"#;
    let mut a = agent("planner", &[list, "read", add, "added", delete, "tried"]);
    for t in ["gcal_list", "gcal_add", "gcal_delete"] {
        a.capabilities.push(Capability::named(t));
    }
    a.auto_approve = vec!["gcal_add".into()]; // the owner's standing yes to adds, and only adds
    rt.create_agent(a).unwrap();
    let ask = || rt.run_agent("planner", "http", "go", &Cause::default(), "", true).unwrap();
    let tool = |r: &RunRecord, name: &str| -> (bool, String) {
        r.steps
            .iter()
            .find_map(|s| match s {
                Step::Tool { name: n, error, result, .. } if n == name => {
                    Some((*error, result.clone()))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no {name} step"))
    };

    let r = ask();
    let (err, out) = tool(&r, "gcal_list");
    assert!(!err && out == "2026-10-15 19:00–20:00  Row  [Training]  (id e1)", "{out}");
    let r = ask();
    let (err, out) = tool(&r, "gcal_add");
    assert!(!err && out.starts_with("added: 2026-10-15 21:00–21:15  Stretch"), "{out}");
    // the delete is sensitive and was not approved: denied, and handler was never called to delete
    let r = ask();
    let (err, out) = tool(&r, "gcal_delete");
    assert!(err, "refused: {out}");
    assert!(
        !seen.lock().unwrap().iter().any(|k| k == "gcal_delete"),
        "nothing was deleted: {:?}",
        seen.lock().unwrap()
    );
}

#[test]
fn a_run_that_misses_a_required_tool_is_retried_on_the_next_model_of_its_chain() {
    let rt = runtime("escalate");
    let mock = |replies: &[&str]| ModelSpec::Mock {
        replies: replies.iter().map(|r| r.to_string()).collect(),
    };
    let mut a = AgentSpec::new("picky", "must remember");
    a.must_call = vec!["remember".into()];
    // the first model answers without the tool, then twice more when nudged; the second does it
    a.model = ModelSpec::Fallback {
        models: vec![
            mock(&["no tool", "still no tool", "nope"]),
            // a mock agent's reply cursor is shared by its runs: the first run used 3
            mock(&["-", "-", "-", r#"{"tool":"remember","args":{"text":"x"}}"#, "done"]),
        ],
    };
    rt.create_agent(a).unwrap();
    let r = rt.run_agent("picky", "http", "go", &Cause::default(), "", true).unwrap();
    assert_eq!((r.status, r.answer.as_str()), (Status::Ok, "done"));
    let runs = rt.store().runs("picky", 5);
    assert_eq!(runs.len(), 2, "the failed attempt is kept, visibly");
    assert_eq!(rt.store().recall("picky", "x", 5).len(), 1);
}

#[test]
fn an_approval_id_from_before_a_restart_never_names_a_call_after_it() {
    let d = dir("approval-ids");
    let asked = |rt: &Runtime| {
        assert!(!rt.approve("a", "run", &serde_json::json!({}), &[]), "nobody answered");
        rt.follow(0)
            .0
            .iter()
            .rev()
            .find_map(|e| match e.event {
                agent_runtime::agent::Event::ApprovalRequested { id, .. } => Some(id),
                _ => None,
            })
            .expect("the approval was announced")
    };
    let cfg = || {
        let mut c = Config::new(&d);
        c.approval_timeout = Duration::from_millis(10);
        c
    };
    let before = asked(&Runtime::new(cfg()).unwrap());
    // The bridge keeps a poll's approval id on disk; after a restart it must not be reused.
    let after = asked(&Runtime::new(cfg()).unwrap());
    assert!(after > before, "{after} after {before}");
}
