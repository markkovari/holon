//! One app, two replicas, two nodes. Does the second replica CONTINUE the first
//! one's count, or start its own?
//!
//! With node-local stores it starts its own — silently — which is the bug this keeps
//! fixed. Both halves matter: the reconciler must REFUSE that arrangement, and a
//! shared store must actually work, or the refusal is just an outage with a reason.
//!
//! This was `bench/adversarial/shared-state.sh`, which shelled out to Python to read
//! a JSON field. Nothing about it needed shell — it is two assertions about a counter.

mod fleet;

use std::time::Duration;

use fleet::Fleet;

/// The rate limiter's remaining budget for one key, from whichever node answers.
fn remaining(fleet: &Fleet, host: &str, key: &str) -> Option<f64> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap();
    let r = client
        .post(format!("http://127.0.0.1:{}/api/ratelimit", fleet.ingress_port))
        .header("host", host)
        .json(&serde_json::json!({ "key": key }))
        .send()
        .ok()?;
    let status = r.status();
    let body = r.text().ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    // A denial still carries the count, and the count is the whole point — reading
    // only 200s would stop looking exactly when the budget runs out.
    // A denial still carries the count, and the count is the whole point — reading
    // only 200s would stop looking exactly when the budget runs out.
    let n = v["remaining"].as_f64();
    if n.is_none() {
        eprintln!("    unreadable: {status} {body}");
    }
    n
}

#[test]
fn a_shared_store_lets_two_replicas_continue_one_count() {
    // `--kv nats` is the default for a lattice node, so both replicas address the
    // same JetStream bucket and the counter is one counter.
    let fleet = Fleet::start("sharedstate", &["fixtures/spread-stateful.yaml"], 2, None);
    assert!(fleet.serves("shop.eve.test", Duration::from_secs(90)), "never served");

    // Spend the budget through the ingress, which spreads across both replicas. The
    // property is that the count keeps FALLING: if each node kept its own store the
    // sequence would jump back up whenever the other one answered.
    let mut seen = Vec::new();
    for _ in 0..6 {
        if let Some(v) = remaining(&fleet, "shop.eve.test", "customer-1") {
            seen.push(v);
        }
    }
    assert!(seen.len() >= 4, "expected several answers, got {seen:?}");
    println!("    remaining, in order: {seen:?}");

    // NOT "it decreases monotonically". A token bucket REFILLS, so the last reading
    // ticks up by a fraction and a strict test fails on a working system — the same
    // refill that was once read as data loss (ADR-0032's list of measurements that
    // meant something else).
    //
    // The split-brain signature is different in kind: a replica with its own store
    // answers from a FULL bucket, so the count jumps back to roughly capacity.
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
    // The other half. With `--kv sqlite` each node has its own file, so two replicas
    // under one bucket name would diverge in silence — nothing errors, the counter
    // just counts wrong and a failover moves the placement without the data.
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
