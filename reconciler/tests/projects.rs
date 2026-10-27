//! Projects and the goal queue, through the real control plane.
//!
//! A lifecycle is only real if the ILLEGAL moves are refused, so most of this tests transitions that
//! must not happen: a goal reaching `done` without running, a dead-lettered goal resurrected, two
//! people starting the same goal. Not tested: what a run does (needs the agent and gate, ADR-0082).

use std::time::Duration;

use holon_fleet::fleet::Fleet;
use serde_json::{json, Value};

/// Guards the window between setting the fleet's env-var config and the fleet reading it. Those vars
/// are PROCESS-global and tests run on parallel threads, so a low `max-placement-lag` would leak
/// into another test's fleet.
static FLEET_CONFIG: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Start a fleet with extra platform config, without leaking it to anyone else.
fn fleet_with(lattice: &str, vars: &[(&str, &str)]) -> Fleet {
    let guard = FLEET_CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    for (k, v) in vars {
        std::env::set_var(k, v);
    }
    let fleet = Fleet::start_with_platform(lattice, 1);
    for (k, _) in vars {
        std::env::remove_var(k);
    }
    drop(guard);
    fleet
}

struct Api {
    base: String,
    http: reqwest::blocking::Client,
    token: String,
}

impl Api {
    fn new(base: String) -> Self {
        let http =
            reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        while std::time::Instant::now() < deadline {
            if http.get(&base).send().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let mut me = Self { base, http, token: String::new() };
        let body = json!({ "email": "ada@projects.test", "password": "password123" });
        let _ = me.raw("/api/register", body.clone());
        let v = me.raw("/api/login", body);
        me.token = v["token"].as_str().unwrap_or_default().to_string();
        assert!(!me.token.is_empty(), "could not log in: {v}");
        me
    }

    fn raw(&self, path: &str, body: Value) -> Value {
        self.http
            .post(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .ok()
            .and_then(|r| r.json().ok())
            .unwrap_or(Value::Null)
    }

    fn post(&self, path: &str, body: Value) -> (u16, Value) {
        match self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
        {
            Ok(r) => (r.status().as_u16(), r.json().unwrap_or(Value::Null)),
            Err(e) => (0, Value::String(format!("transport: {e}"))),
        }
    }

    fn get(&self, path: &str) -> (u16, Value) {
        match self.http.get(format!("{}{path}", self.base)).bearer_auth(&self.token).send() {
            Ok(r) => (r.status().as_u16(), r.json().unwrap_or(Value::Null)),
            Err(e) => (0, Value::String(format!("transport: {e}"))),
        }
    }

    fn delete(&self, path: &str) -> (u16, Value) {
        match self.http.delete(format!("{}{path}", self.base)).bearer_auth(&self.token).send() {
            Ok(r) => (r.status().as_u16(), r.json().unwrap_or(Value::Null)),
            Err(e) => (0, Value::String(format!("transport: {e}"))),
        }
    }

    /// Post a fleet status as the reconciler does, so admission is tested against a number.
    fn status(&self, body: Value) -> u16 {
        self.http
            .post(format!("{}/api/internal/status", self.base))
            .header("x-platform-secret", "test-secret")
            .json(&body)
            .send()
            .map(|r| r.status().as_u16())
            .unwrap_or(0)
    }

    fn goal(&self, project: &str, title: &str) -> String {
        let (code, v) =
            self.post(&format!("/api/projects/{project}/goals"), json!({ "title": title }));
        assert_eq!(code, 201, "queueing `{title}` failed: {v}");
        v["id"].as_str().unwrap().to_string()
    }

    fn part(&self, project: &str, title: &str, parent: &str) -> (u16, Value) {
        self.post(
            &format!("/api/projects/{project}/goals"),
            json!({ "title": title, "parent": parent }),
        )
    }

    fn state_of(&self, project: &str, id: &str) -> String {
        let (_, v) = self.get(&format!("/api/projects/{project}/goals"));
        v["goals"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .find(|g| g["id"].as_str() == Some(id))
            .map(|g| g["state"].as_str().unwrap_or_default().to_string())
            .unwrap_or_else(|| "(gone)".into())
    }
}

#[test]
fn a_queue_that_only_a_person_can_start_and_that_refuses_illegal_moves() {
    let fleet = Fleet::start_with_platform("projects", 1);
    let api = Api::new(fleet.platform_url());

    let (code, p) = api.post("/api/projects", json!({ "name": "widgets", "repo": "acme/widgets" }));
    assert_eq!(code, 201, "creating a project failed: {p}");
    assert_eq!(p["base"], json!("main"), "the base should default: {p}");

    // A name unfit for a store or branch name, and a repo that is not `owner/name`, are refused HERE
    // rather than at the first forge call, which answers 404.
    for bad in [
        json!({ "name": "Widgets", "repo": "acme/widgets" }),
        json!({ "name": "-lead", "repo": "acme/widgets" }),
        json!({ "name": "ok", "repo": "widgets" }),
        json!({ "name": "ok", "repo": "a/b/c" }),
    ] {
        let (code, v) = api.post("/api/projects", bad.clone());
        assert_eq!(code, 422, "{bad} should have been refused: {v}");
    }
    let (code, v) = api.post("/api/projects", json!({ "name": "widgets", "repo": "acme/other" }));
    assert_eq!(code, 409, "a duplicate project name should conflict: {v}");

    let cache = api.goal("widgets", "add a cache");
    let rename = api.goal("widgets", "rename the thing");
    let doomed = api.goal("widgets", "something impossible");

    let (_, v) = api.get("/api/projects/widgets/goals");
    assert_eq!(v["count"], json!(3), "three goals should be queued: {v}");
    assert!(
        v["goals"].as_array().unwrap().iter().all(|g| g["state"] == json!("queued")),
        "everything starts queued and STAYS there — nothing drains this: {v}"
    );

    // The queue does not move on its own; waiting proves it.
    std::thread::sleep(Duration::from_secs(6));
    assert_eq!(
        api.state_of("widgets", &cache),
        "queued",
        "something started a goal without being asked — a human starts every goal"
    );

    let (code, v) = api.post(&format!("/api/goals/{cache}/start"), json!({}));
    assert_eq!(code, 200, "starting failed: {v}");
    assert_eq!(v["from"], json!("queued"));
    assert_eq!(api.state_of("widgets", &cache), "running");

    // Straight to done without ever having been reviewed.
    let (code, v) = api.post(&format!("/api/goals/{cache}/done"), json!({}));
    assert_eq!(code, 409, "a running goal must not jump to done: {v}");

    // Started twice: the case the design exists to prevent, and the record's revision prevents it.
    let (code, v) = api.post(&format!("/api/goals/{cache}/start"), json!({}));
    assert_eq!(code, 409, "a goal already running must not start again: {v}");

    let (code, v) = api.post(&format!("/api/goals/{cache}/review"), json!({}));
    assert_eq!(code, 200, "running -> awaiting-human should be legal: {v}");
    let (code, v) = api.post(&format!("/api/goals/{cache}/done"), json!({}));
    assert_eq!(code, 200, "awaiting-human -> done should be legal: {v}");
    assert_eq!(api.state_of("widgets", &cache), "done");

    let (code, _) = api.post(&format!("/api/goals/{doomed}/start"), json!({}));
    assert_eq!(code, 200);
    let (code, v) = api.post(
        &format!("/api/goals/{doomed}/fail"),
        json!({ "reason": "the spec asked for something that cannot exist" }),
    );
    assert_eq!(code, 200, "failing should be allowed: {v}");

    let (_, v) = api.get("/api/projects/widgets/goals?state=failed");
    assert_eq!(v["count"], json!(1), "the dead-letter queue should hold it: {v}");
    let dead = &v["goals"][0];
    assert!(
        dead["reason"].as_str().unwrap_or_default().contains("cannot exist"),
        "a dead letter with no reason is one nobody can act on: {dead}"
    );

    // Nothing leaves `failed`. A retry is a NEW goal, so resurrecting this one would erase history.
    for to in ["start", "review", "done"] {
        let (code, v) = api.post(&format!("/api/goals/{doomed}/{to}"), json!({}));
        assert_eq!(code, 409, "a dead-lettered goal must not be resurrected via {to}: {v}");
    }

    let (code, v) = api.delete(&format!("/api/goals/{rename}"));
    assert_eq!(code, 200, "abandoning a queued goal should work: {v}");
    assert_eq!(api.state_of("widgets", &rename), "abandoned");

    let (_, v) = api.get("/api/projects");
    let widgets = v["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == json!("widgets"))
        .cloned()
        .unwrap();
    assert_eq!(widgets["queued"], json!(0), "nothing left queued: {widgets}");
    assert_eq!(widgets["failed"], json!(1), "one dead letter: {widgets}");
    assert_eq!(widgets["repo"], json!("acme/widgets"));

    // A goal for a project that does not exist is a 404, not a goal filed under a typo.
    let (code, v) = api.post("/api/projects/nosuch/goals", json!({ "title": "x" }));
    assert_eq!(code, 404, "a goal needs a project that exists: {v}");

    println!("    a queue nothing drains, and six illegal transitions refused");
}

/// A review can name the pull request it is waiting on, so a human does not guess a branch name and
/// something can later watch that PR.
#[test]
fn a_review_can_name_the_pull_request_it_is_waiting_on() {
    let fleet = Fleet::start_with_platform("projects-pr", 1);
    let api = Api::new(fleet.platform_url());
    api.post("/api/projects", json!({ "name": "widgets", "repo": "acme/widgets" }));

    let cache = api.goal("widgets", "add a cache");
    api.post(&format!("/api/goals/{cache}/start"), json!({}));

    let (code, v) = api.post(
        &format!("/api/goals/{cache}/review"),
        json!({ "pr": "https://github.com/acme/widgets/pull/42" }),
    );
    assert_eq!(code, 200, "running -> awaiting-human should still be legal with a pr: {v}");

    let (_, v) = api.get("/api/projects/widgets/goals");
    let goal = v["goals"].as_array().unwrap().iter().find(|g| g["id"].as_str() == Some(&cache));
    assert_eq!(
        goal.unwrap()["pr"],
        json!("https://github.com/acme/widgets/pull/42"),
        "the pull request should be recorded on the goal: {v}"
    );

    // A caller with nothing to name still transitions cleanly: the field is additive.
    let plain = api.goal("widgets", "rename the thing");
    api.post(&format!("/api/goals/{plain}/start"), json!({}));
    let (code, v) = api.post(&format!("/api/goals/{plain}/review"), json!({}));
    assert_eq!(code, 200, "a review with no pr should still be legal: {v}");
    let (_, v) = api.get("/api/projects/widgets/goals");
    let goal = v["goals"].as_array().unwrap().iter().find(|g| g["id"].as_str() == Some(&plain));
    assert_eq!(goal.unwrap().get("pr"), None, "no pr named, none should be recorded: {v}");

    println!("    a review names its pull request, and stays optional");
}

/// The event log is a fast, lossy-hint path over the same transitions, never a second place the
/// truth lives. It shows up on the project's event topic in order, acking moves a consumer group's
/// offset, and `goals_list` is unaffected by whether anyone was watching.
#[test]
fn goal_transitions_appear_on_the_event_log_and_acking_advances_the_offset() {
    let fleet = Fleet::start_with_platform("projects-events", 1);
    let api = Api::new(fleet.platform_url());
    api.post("/api/projects", json!({ "name": "widgets", "repo": "acme/widgets" }));

    let cache = api.goal("widgets", "add a cache");
    api.post(&format!("/api/goals/{cache}/start"), json!({}));
    api.post(
        &format!("/api/goals/{cache}/review"),
        json!({ "pr": "https://github.com/acme/widgets/pull/7" }),
    );

    let (code, v) = api.get("/api/projects/widgets/events?group=watcher");
    assert_eq!(code, 200, "polling the event log failed: {v}");
    let events = v["events"].as_array().cloned().unwrap_or_default();
    assert_eq!(events.len(), 3, "queued, started and reviewed should all appear: {v}");
    let transitions: Vec<(String, String)> = events
        .iter()
        .map(|e| {
            (
                e["event"]["from"].as_str().unwrap_or("null").to_string(),
                e["event"]["to"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    assert_eq!(
        transitions,
        vec![
            ("null".into(), "queued".into()),
            ("queued".into(), "running".into()),
            ("running".into(), "awaiting-human".into()),
        ],
        "events should appear in the order they happened: {v}"
    );
    assert_eq!(
        events[2]["event"]["id"].as_str(),
        Some(cache.as_str()),
        "the event should name the goal it happened to: {v}"
    );

    // Ack everything, then poll with the SAME group: nothing comes back.
    let ids: Vec<String> =
        events.iter().map(|e| e["id"].as_str().unwrap_or_default().to_string()).collect();
    let (code, v) =
        api.post("/api/projects/widgets/events/ack", json!({ "group": "watcher", "ids": ids }));
    assert_eq!(code, 200, "acking failed: {v}");
    assert_eq!(v["acked"], json!(3));

    let (_, v) = api.get("/api/projects/widgets/events?group=watcher");
    assert_eq!(v["count"], json!(0), "an acked group should see nothing new: {v}");

    // A DIFFERENT group starting fresh still sees the whole history, as in `event:bus`'s contract.
    let (_, v) = api.get("/api/projects/widgets/events?group=late-watcher");
    assert_eq!(v["count"], json!(3), "a new group should see the full backlog: {v}");

    println!("    transitions appear on the event log, in order, and acking advances the offset");
}

/// The reconciler actually SENDS the numbers admission reads.
///
/// Other admission tests post their own body, which says nothing about the writer: `report()`
/// once took `lag` and `nodes` and posted neither, so `fleet_lag()` read 0 and admission never
/// fired. Asserted against the SOURCES because it is a disagreement between two files.
#[test]
fn the_reconciler_reports_the_numbers_admission_reads() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let read = |p: &str| std::fs::read_to_string(root.join(p)).unwrap();

    let platform = read("components/platform-domain/src/lib.rs");
    let reconciler = read("reconciler/src/main.rs");

    // Whatever `fleet_lag()` pulls off the stored row, `report()` has to put there.
    let fleet_lag = platform
        .split("fn fleet_lag()")
        .nth(1)
        .expect("platform-domain no longer has fleet_lag — admission moved");
    let fleet_lag = &fleet_lag[..fleet_lag.find("\n}").unwrap_or(fleet_lag.len())];

    let reported = reconciler
        .split("/api/internal/status")
        .nth(1)
        .expect("the reconciler no longer posts a fleet status");

    for field in ["lag", "nodes"] {
        assert!(
            fleet_lag.contains(&format!("row[\"{field}\"]")),
            "fleet_lag stopped reading `{field}` — drop it from this list, and from the report"
        );
        assert!(
            reported.contains(&format!("\"{field}\":")),
            "admission reads `{field}` off the fleet row and the reconciler does not send it, \
             so it is permanently whatever `unwrap_or` says"
        );
    }
}

/// The fleet refuses work it could not possibly place.
///
/// A stress run was accepted 3125 of 3906 environments while nothing was being placed; accepting
/// work that cannot be done is worse than refusing it. The LAG is driven through the internal
/// endpoint the reconciler posts to, since a fleet with no desired state has no lag to wait for.
#[test]
fn the_platform_refuses_work_the_fleet_cannot_place() {
    let fleet = fleet_with("admission", &[("COMP_MAX_PLACEMENT_LAG", "10")]);
    let api = Api::new(fleet.platform_url());

    let (code, p) = api.post("/api/projects", json!({ "name": "load", "repo": "acme/load" }));
    assert_eq!(code, 201, "creating a project failed: {p}");

    // Caught up: a spawn reaches the 404 path, proving admission did NOT refuse it.
    assert_eq!(api.status(json!({ "lag": 0, "desired": 0, "placed": 0 })), 200);
    let (code, body) = api.post("/api/environments", json!({ "app": "anything", "env": "x" }));
    assert_eq!(code, 404, "a caught-up fleet must admit the request: {body}");

    assert_eq!(api.status(json!({ "lag": 5000, "desired": 5000, "placed": 0 })), 200);
    let (code, body) = api.post("/api/environments", json!({ "app": "anything", "env": "x" }));
    assert_eq!(
        code, 429,
        "the platform accepted work while the fleet was 5000 behind — every one of \
         those is recorded, reported as created, and never started: {body}"
    );
    let detail = body["error"].as_str().unwrap_or_default().to_string();
    assert!(
        detail.contains("5000") && detail.contains("10"),
        "a refusal has to say how far behind and what the limit is, or nobody can act \
         on it: {body}"
    );

    // And it lets go once the fleet catches up, rather than latching.
    assert_eq!(api.status(json!({ "lag": 1, "desired": 5000, "placed": 4999 })), 200);
    let (code, body) = api.post("/api/environments", json!({ "app": "anything", "env": "x" }));
    assert_eq!(code, 404, "a caught-up fleet must be admitted again: {body}");

    // --- a BURST cannot outrun the report ---
    // Admission is only as fresh as the last report: a stress run fired 625 spawns in 0.2 s against a
    // limit of 200 and all were admitted, so what has been let through since is counted.
    assert_eq!(api.status(json!({ "lag": 0, "desired": 0, "placed": 0 })), 200);
    let mut admitted = 0;
    let mut refused_at = None;
    for i in 0..40 {
        let (code, body) = api.post("/api/environments", json!({ "app": "anything", "env": "x" }));
        match code {
            404 => admitted += 1,
            429 => {
                refused_at = Some((i, body));
                break;
            }
            other => panic!("unexpected {other}: {body}"),
        }
    }
    let (at, why) = refused_at.expect(
        "forty spawns went through on one stale report saying the fleet was caught up — \
         admission that only counts what the reconciler last said cannot see a burst",
    );
    assert!(
        at <= 12,
        "the limit is 10 and {admitted} were admitted before the first refusal at {at}"
    );
    assert!(
        why["error"].as_str().unwrap_or_default().contains("since it last reported"),
        "the refusal should say how much was accepted since the report: {why}"
    );
    println!("    a burst was cut off after {admitted} against a limit of 10");

    // --- the limit scales with the fleet ---
    // A flat number is wrong everywhere but where it was measured, so the limit is per-node. This
    // platform has the flat override, so scaling is checked on a second one.
    {
        let big = fleet_with("scaling", &[("COMP_MAX_PLACEMENT_LAG_PER_NODE", "5")]);
        let api = Api::new(big.platform_url());
        let (code, _) = api.post("/api/projects", json!({ "name": "s", "repo": "a/b" }));
        assert_eq!(code, 201);

        // One node: a lag of 8 is over a budget of 5.
        assert_eq!(api.status(json!({ "lag": 8, "nodes": 1 })), 200);
        let (code, body) = api.post("/api/environments", json!({ "app": "x", "env": "e" }));
        assert_eq!(code, 429, "8 behind on one node exceeds 5 per node: {body}");

        // The same backlog across four nodes is well within budget.
        assert_eq!(api.status(json!({ "lag": 8, "nodes": 4 })), 200);
        let (code, body) = api.post("/api/environments", json!({ "app": "x", "env": "e" }));
        assert_eq!(
            code, 404,
            "8 behind across four nodes is inside a budget of 20, so it must be admitted \
             — a limit that does not grow with the fleet throttles the fleet it has: {body}"
        );
        println!("    the limit scales: 8 behind refused on 1 node, admitted on 4");
    }

    // --- and the REAL reconciler reports it ---
    // The endpoint once did not exist and the reconciler posted into a 404 unnoticed, so
    // `unschedulable` and `at_ceiling` were reported into the void.
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    let mut reported = false;
    while std::time::Instant::now() < deadline {
        // A report from the loop overwrites the one this test posted; `desired` is only set by the reconciler.
        if !fleet.reconciler_log().contains("could not report status")
            && !fleet.reconciler_log().contains("refused the status report")
        {
            // Nothing refused it; confirm something arrived by watching the lag move to the real fleet's.
            let (code, _) = api.post("/api/environments", json!({ "app": "anything", "env": "y" }));
            if code == 404 {
                reported = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    assert!(
        reported,
        "the reconciler's own status never landed — it reported into a 404 for a long \
         time and the error was discarded, which is exactly what this asserts against\n\
         --- reconciler ---\n{}",
        fleet.reconciler_log()
    );
    assert!(
        !fleet.reconciler_log().contains("refused the status report"),
        "the platform refused the reconciler's status report:\n{}",
        fleet.reconciler_log()
    );

    println!("    refused at lag 5000, admitted at lag 1, and the loop reports its own");
}

/// A stale fleet report fails CLOSED: with the reconciler stopped, nothing will place new work, and
/// failing open would accept unboundedly (ADR-0081). `status-max-age` is set below the reconcile
/// interval so the refusal can be observed without stopping anything.
#[test]
fn a_stale_fleet_report_stops_new_work() {
    let fleet =
        fleet_with("stale", &[("COMP_MAX_PLACEMENT_LAG", "10000"), ("COMP_STATUS_MAX_AGE", "1")]);
    let api = Api::new(fleet.platform_url());

    // A fresh report is admitted: the limit is enormous, so only age can refuse.
    assert_eq!(api.status(json!({ "lag": 0, "desired": 0, "placed": 0 })), 200);
    let (code, body) = api.post("/api/environments", json!({ "app": "anything", "env": "x" }));
    assert_eq!(code, 404, "a fresh report must be admitted: {body}");

    // Let it go stale; poll for the window since the reconciler refreshes every few seconds.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut refused = Value::Null;
    let mut saw = false;
    while std::time::Instant::now() < deadline {
        let (code, body) = api.post("/api/environments", json!({ "app": "anything", "env": "x" }));
        if code == 503 {
            refused = body;
            saw = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(
        saw,
        "a stale report was never refused — admission fails OPEN, which means unbounded \
         acceptance exactly when nothing is placing work\n--- reconciler ---\n{}",
        fleet.reconciler_log()
    );
    let detail = refused["error"].as_str().unwrap_or_default().to_string();
    assert!(
        detail.contains("reported") && detail.contains("stale"),
        "a refusal must say the loop has gone quiet, or it reads as the fleet being \
         full: {refused}"
    );
    println!("    stale report refused: {detail}");
}

/// A sub-goal is an ordinary goal with a parent, and the queue keeps that honest.
///
/// The pool holds the same relationship as a `decomposes_into` edge keyed by goal TEXT (what has
/// been learned); the queue is what is still to do. Mostly REFUSALS are asserted: a part under a
/// parent in another project, a cycle or runaway chain, and a parent finished under live parts.
#[test]
fn a_sub_goal_is_a_goal_with_a_parent_and_the_queue_refuses_the_shapes_that_are_not() {
    let fleet = Fleet::start_with_platform("subgoals", 1);
    let api = Api::new(fleet.platform_url());
    for (name, repo) in [("widgets", "acme/widgets"), ("gadgets", "acme/gadgets")] {
        let (code, v) = api.post("/api/projects", json!({ "name": name, "repo": repo }));
        assert_eq!(code, 201, "creating `{name}` failed: {v}");
    }

    let parent = api.goal("widgets", "serve invoices over http");

    let (code, back) = api.part("widgets", "store invoices in postgres", &parent);
    assert_eq!(code, 201, "a part of a goal in the same project is ordinary: {back}");
    let (code, front) = api.part("widgets", "render the invoice list", &parent);
    assert_eq!(code, 201, "{front}");
    let back = back["id"].as_str().unwrap().to_string();

    // `?parent=<id>` is a goal's parts, `?parent=` is what is nobody's part, and no `parent` is
    // everything (what callers that predate it must keep getting).
    let (_, parts) = api.get(&format!("/api/projects/widgets/goals?parent={parent}"));
    assert_eq!(parts["count"], json!(2), "both parts: {parts}");
    let (_, top) = api.get("/api/projects/widgets/goals?parent=");
    assert_eq!(top["count"], json!(1), "only the goal that is nobody's part: {top}");
    let (_, all) = api.get("/api/projects/widgets/goals");
    assert_eq!(all["count"], json!(3), "absent means every goal: {all}");

    let (code, v) = api.part("widgets", "an orphan", "goals:does-not-exist");
    assert_eq!(code, 422, "a parent that is not there must be refused: {v}");

    let elsewhere = api.goal("gadgets", "a goal in another project");
    let (code, v) = api.part("widgets", "a part across projects", &elsewhere);
    assert_eq!(code, 422, "a worklist is per project: {v}");
    assert!(
        v["error"].as_str().unwrap_or_default().contains("gadgets"),
        "the refusal has to name the project that owns the parent: {v}"
    );

    // Depth: eight levels is the bound; the ninth is refused and the message must say WHY.
    let mut chain = api.goal("widgets", "level 0");
    for i in 1..8 {
        let (code, v) = api.part("widgets", &format!("level {i}"), &chain);
        assert_eq!(code, 201, "level {i} should still fit: {v}");
        chain = v["id"].as_str().unwrap().to_string();
    }
    let (code, v) = api.part("widgets", "one level too far", &chain);
    assert_eq!(code, 422, "the ninth level must be refused: {v}");
    assert!(
        v["error"].as_str().unwrap_or_default().contains("deep"),
        "and say it is depth, not a missing parent: {v}"
    );

    api.post(&format!("/api/goals/{parent}/start"), json!({}));
    api.post(&format!("/api/goals/{parent}/review"), json!({}));
    let (code, v) = api.post(&format!("/api/goals/{parent}/done"), json!({}));
    assert_eq!(code, 409, "two parts are still queued: {v}");
    let detail = v["error"].as_str().unwrap_or_default().to_string();
    assert!(
        detail.contains("unfinished part"),
        "the refusal must say what is holding it: {detail}"
    );
    assert!(
        detail.contains("store invoices in postgres"),
        "and name them, or the caller has to go looking: {detail}"
    );

    // Finish the parts and the parent may finish. `failed` counts as finished: a parent held open
    // forever by an immovable part is worse.
    {
        let p = &back;
        api.post(&format!("/api/goals/{p}/start"), json!({}));
        api.post(&format!("/api/goals/{p}/review"), json!({}));
        api.post(&format!("/api/goals/{p}/done"), json!({}));
    }
    let front_id = front["id"].as_str().unwrap();
    api.delete(&format!("/api/goals/{front_id}"));
    let (code, v) = api.post(&format!("/api/goals/{parent}/done"), json!({}));
    assert_eq!(code, 200, "every part is terminal now: {v}");
    assert_eq!(api.state_of("widgets", &parent), "done");
}
