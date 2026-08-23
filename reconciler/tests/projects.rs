//! Projects and the goal queue, through the real control plane.
//!
//! The lifecycle is the whole feature, and a lifecycle is only real if the
//! ILLEGAL moves are refused. A state field that anything may set to anything is
//! a state field, not a state machine — so most of this test is about the
//! transitions that must not happen: a goal reaching `done` without running, a
//! dead-lettered goal being quietly resurrected, two people starting the same
//! goal and both believing they own it.
//!
//! What is deliberately NOT here: a goal that actually does anything. Starting
//! one records that it started; what a run *does* needs the agent and the gate,
//! which do not exist yet (ADR-0082). Testing the queue is honest; pretending
//! there is something behind it would not be.

use std::time::Duration;

use comp_reconciler::fleet::Fleet;
use serde_json::{json, Value};

/// Guards the window between setting the fleet's env-var config and the fleet
/// having read it.
///
/// Those vars are PROCESS-global and tests in one binary run on parallel
/// threads, so without this a test that sets `max-placement-lag` low leaks it
/// into a fleet another test is starting at that moment — and the other test
/// fails for a reason that is nowhere in its own source. That happened
/// immediately: three tests passing alone and one failing when run together.
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

    /// Post a fleet status as the reconciler does, so admission can be tested
    /// against a number instead of against the weather.
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

    // --- a project owns one repo --------------------------------------------
    let (code, p) = api.post("/api/projects", json!({ "name": "widgets", "repo": "acme/widgets" }));
    assert_eq!(code, 201, "creating a project failed: {p}");
    assert_eq!(p["base"], json!("main"), "the base should default: {p}");

    // A name that would not survive being part of a store or branch name, and a
    // repo that is not `owner/name`, are refused HERE — where the message can say
    // so — rather than at the first forge call, which answers 404.
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

    // --- the queue ----------------------------------------------------------
    let cache = api.goal("widgets", "add a cache");
    let rename = api.goal("widgets", "rename the thing");
    let doomed = api.goal("widgets", "something impossible");

    let (_, v) = api.get("/api/projects/widgets/goals");
    assert_eq!(v["count"], json!(3), "three goals should be queued: {v}");
    assert!(
        v["goals"].as_array().unwrap().iter().all(|g| g["state"] == json!("queued")),
        "everything starts queued and STAYS there — nothing drains this: {v}"
    );

    // The queue does not move on its own. Waiting proves it: a loop that drained
    // would have taken something by now, and this design deliberately has none.
    std::thread::sleep(Duration::from_secs(6));
    assert_eq!(
        api.state_of("widgets", &cache),
        "queued",
        "something started a goal without being asked — a human starts every goal"
    );

    // --- the one transition a person makes ----------------------------------
    let (code, v) = api.post(&format!("/api/goals/{cache}/start"), json!({}));
    assert_eq!(code, 200, "starting failed: {v}");
    assert_eq!(v["from"], json!("queued"));
    assert_eq!(api.state_of("widgets", &cache), "running");

    // --- the illegal moves, which are the point -----------------------------
    // Straight to done without ever having been reviewed.
    let (code, v) = api.post(&format!("/api/goals/{cache}/done"), json!({}));
    assert_eq!(code, 409, "a running goal must not jump to done: {v}");

    // Started twice. With one run per project this is the case the whole design
    // exists to prevent, and the record's revision is what prevents it.
    let (code, v) = api.post(&format!("/api/goals/{cache}/start"), json!({}));
    assert_eq!(code, 409, "a goal already running must not start again: {v}");

    // The legal path through review.
    let (code, v) = api.post(&format!("/api/goals/{cache}/review"), json!({}));
    assert_eq!(code, 200, "running -> awaiting-human should be legal: {v}");
    let (code, v) = api.post(&format!("/api/goals/{cache}/done"), json!({}));
    assert_eq!(code, 200, "awaiting-human -> done should be legal: {v}");
    assert_eq!(api.state_of("widgets", &cache), "done");

    // --- the dead-letter queue is terminal ----------------------------------
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

    // Nothing leaves `failed`. A retry is a NEW goal, so what was tried stays
    // visible — resurrecting this one would erase the history of the attempt.
    for to in ["start", "review", "done"] {
        let (code, v) = api.post(&format!("/api/goals/{doomed}/{to}"), json!({}));
        assert_eq!(code, 409, "a dead-lettered goal must not be resurrected via {to}: {v}");
    }

    // --- abandoning something never started ---------------------------------
    let (code, v) = api.delete(&format!("/api/goals/{rename}"));
    assert_eq!(code, 200, "abandoning a queued goal should work: {v}");
    assert_eq!(api.state_of("widgets", &rename), "abandoned");

    // --- what the listing tells a person ------------------------------------
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

    // A goal for a project that does not exist is a 404, not a goal filed under a
    // typo that nobody will ever look at.
    let (code, v) = api.post("/api/projects/nosuch/goals", json!({ "title": "x" }));
    assert_eq!(code, 404, "a goal needs a project that exists: {v}");

    println!("    a queue nothing drains, and six illegal transitions refused");
}

/// The reconciler actually SENDS the numbers admission reads.
///
/// Every other admission test posts to `/api/internal/status` itself, with a body
/// it wrote — which proves the platform's half and says nothing about the writer's.
/// It said nothing for a long time: `report()` computed `lag` and `nodes`, took
/// them as arguments, and posted a body containing neither. `fleet_lag()` therefore
/// read a lag that was permanently 0 and a node count that permanently defaulted to
/// 1, so the per-node limit was never per-node and the lag half of admission never
/// fired. rustc had been calling those arguments unused the whole time.
///
/// Asserted against the SOURCES rather than by running a fleet, for the same reason
/// `capgraph_store.rs` checks its coordinates that way: this is a disagreement
/// between two files, and it survived precisely because every test that could see
/// it supplied the body itself.
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
/// A stress run grew 3906 environments and watched the platform accept 3125 of
/// them in 1.4 seconds while nothing was being placed — every one recorded, every
/// one reported to the caller as created, none ever started. Accepting work that
/// cannot be done is worse than refusing it: a refusal is actionable and a
/// phantom app is not.
///
/// The LAG is driven through the internal endpoint the reconciler posts to,
/// rather than by growing a real backlog. That is deliberate. The first version
/// of this test set the limit to 1 and waited for a fleet with nothing deployed
/// to fall behind — which it never did, correctly, because a fleet with no
/// desired state has no lag. It waited two minutes and proved nothing. Driving
/// the contract directly tests the thing that failed in the stress run:
/// admission, given a number.
#[test]
fn the_platform_refuses_work_the_fleet_cannot_place() {
    let fleet = fleet_with("admission", &[("COMP_MAX_PLACEMENT_LAG", "10")]);
    let api = Api::new(fleet.platform_url());

    let (code, p) = api.post("/api/projects", json!({ "name": "load", "repo": "acme/load" }));
    assert_eq!(code, 201, "creating a project failed: {p}");

    // Caught up: a spawn gets as far as the app not existing, which is the 404
    // path and proves admission did NOT refuse it.
    assert_eq!(api.status(json!({ "lag": 0, "desired": 0, "placed": 0 })), 200);
    let (code, body) = api.post("/api/environments", json!({ "app": "anything", "env": "x" }));
    assert_eq!(code, 404, "a caught-up fleet must admit the request: {body}");

    // Behind: refused before anything is written.
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

    // And it lets go again once the fleet catches up, rather than latching.
    assert_eq!(api.status(json!({ "lag": 1, "desired": 5000, "placed": 4999 })), 200);
    let (code, body) = api.post("/api/environments", json!({ "app": "anything", "env": "x" }));
    assert_eq!(code, 404, "a caught-up fleet must be admitted again: {body}");

    // --- a BURST cannot outrun the report -------------------------------------
    //
    // Admission is only as fresh as the last report, so without counting what has
    // been let through since, a burst faster than the reporting interval sails
    // straight past the limit. That is not hypothetical: a stress run fired 625
    // spawns in 0.2 seconds against a limit of 200 and every single one was
    // admitted, because the newest number the platform had was seconds old and
    // said the fleet was nearly caught up.
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

    // --- the limit scales with the fleet --------------------------------------
    //
    // A flat number is wrong everywhere except where it was measured: the same
    // backlog that is reasonable across ten nodes is absurd on one. So the limit
    // is per-node, and a bigger fleet is allowed to be further behind.
    //
    // This fleet's platform is configured with the flat override, so the scaling
    // is checked on a second one.
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

    // --- and the REAL reconciler reports it, not just this test ---------------
    //
    // The endpoint did not exist until now and the reconciler posted into a 404
    // with the result discarded, so `unschedulable` and `at_ceiling` had been
    // reported into the void since they were written. A test that only drives the
    // contract by hand would not have noticed that, and would not notice it
    // breaking again.
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    let mut reported = false;
    while std::time::Instant::now() < deadline {
        // A report from the loop overwrites the one this test posted; `desired`
        // is the field only the reconciler sets from real manifests.
        if !fleet.reconciler_log().contains("could not report status")
            && !fleet.reconciler_log().contains("refused the status report")
        {
            // Nothing refused it. Confirm something actually arrived by watching
            // the lag move back to what the real fleet says.
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

/// A stale fleet report fails CLOSED.
///
/// If the reconciler has stopped, accepting more work is pointless — nothing will
/// place it — and failing open would mean unbounded acceptance at exactly the
/// moment nothing is being done. That reasoning was written down when admission
/// was built and nothing checked it, which by this repo's own rule (ADR-0081)
/// makes it documentation.
///
/// `status-max-age` is set below the reconcile interval, so the report spends
/// most of each cycle stale and the refusal can be observed without stopping
/// anything.
#[test]
fn a_stale_fleet_report_stops_new_work() {
    let fleet =
        fleet_with("stale", &[("COMP_MAX_PLACEMENT_LAG", "10000"), ("COMP_STATUS_MAX_AGE", "1")]);
    let api = Api::new(fleet.platform_url());

    // A fresh report is admitted: the limit is enormous, so only age can refuse.
    assert_eq!(api.status(json!({ "lag": 0, "desired": 0, "placed": 0 })), 200);
    let (code, body) = api.post("/api/environments", json!({ "app": "anything", "env": "x" }));
    assert_eq!(code, 404, "a fresh report must be admitted: {body}");

    // Let it go stale. The reconciler refreshes every few seconds, so this polls
    // for the window rather than assuming one.
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
