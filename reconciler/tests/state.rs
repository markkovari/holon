//! One app, two replicas, two nodes. Does the second replica CONTINUE the first one's count, or start
//! its own?
//!
//! With node-local stores it starts its own, silently. Both halves matter: the reconciler must REFUSE
//! that arrangement, and a shared store must actually work, or the refusal is an outage with a reason.

use std::time::Duration;

use holon_fleet::fleet::Fleet;

/// The rate limiter's remaining budget for one key, from whichever node answers.
fn remaining(fleet: &Fleet, host: &str, key: &str) -> Option<f64> {
    let client =
        reqwest::blocking::Client::builder().timeout(Duration::from_secs(8)).build().unwrap();
    let r = client
        .post(format!("http://127.0.0.1:{}/api/ratelimit", fleet.ingress_port))
        .header("host", host)
        .json(&serde_json::json!({ "key": key }))
        .send()
        .ok()?;
    let status = r.status();
    let body = r.text().ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    // A denial still carries the count, and reading only 200s would stop looking exactly when the
    // budget runs out.
    let n = v["remaining"].as_f64();
    if n.is_none() {
        eprintln!("    unreadable: {status} {body}");
    }
    n
}

#[test]
fn a_shared_store_lets_two_replicas_continue_one_count() {
    // `--kv nats` is the default for a lattice node, so both replicas share one JetStream bucket.
    let fleet = Fleet::start("sharedstate", &["fixtures/spread-stateful.yaml"], 2, None);
    // 150s, not 90: this spins up TWO full replicas, and a loaded CI runner has taken 94s.
    assert!(fleet.serves("shop.eve.test", Duration::from_secs(150)), "never served");

    // Spend the budget through the ingress, which spreads across both replicas: the count must keep
    // FALLING (per-node stores would jump back up whenever the other node answered).
    let mut seen = Vec::new();
    for _ in 0..6 {
        if let Some(v) = remaining(&fleet, "shop.eve.test", "customer-1") {
            seen.push(v);
        }
    }
    assert!(seen.len() >= 4, "expected several answers, got {seen:?}");
    println!("    remaining, in order: {seen:?}");

    // NOT "it decreases monotonically": a token bucket REFILLS, so a strict test fails on a working
    // system (ADR-0032). The split-brain signature is a replica answering from a FULL bucket, so the
    // count jumps back to roughly capacity.
    let start = seen[0];
    let worst = seen[1..].iter().cloned().fold(f64::MIN, f64::max);
    assert!(
        worst < start,
        "a reading came back at or above the starting budget, so a replica was \
         answering from its own store: {seen:?}"
    );
    assert!(
        seen[seen.len() - 1] < start / 2.0,
        "the budget barely moved, so the replicas may not be sharing one count: {seen:?}"
    );
}

#[test]
fn spreading_a_stateful_app_over_node_local_stores_is_refused() {
    // The other half: with `--kv sqlite` each node has its own file, so two replicas under one bucket
    // name diverge in silence and a failover moves the placement without the data.
    let fleet = Fleet::start_with_kv(
        "localstate",
        &["fixtures/spread-stateful.yaml"],
        2,
        None,
        Some("sqlite"),
    );

    // Nothing may be placed, and the reason has to name the fix.
    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    let mut reason = String::new();
    while std::time::Instant::now() < deadline {
        reason = fleet.reconciler_log();
        if reason.contains("unschedulable") {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(reason.contains("unschedulable"), "it was placed anyway:\n{reason}");
    for expected in ["node-local", "--kv nats"] {
        assert!(
            reason.contains(expected),
            "the refusal should tell an operator how to fix it, missing {expected:?}:\n{reason}"
        );
    }
    assert_eq!(fleet.replicas(), 0, "a refused app must not be half-placed");
    println!("    refused, and the reason names the fix");
}
