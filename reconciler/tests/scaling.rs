//! Scaling, end to end: up under load, back down when idle, and up again when the
//! ingress is refusing traffic.
//!
//! This replaces `bench/autoscale/` and `bench/shedscale/`, which were two bash
//! scripts and two Python watchers asking almost the same question. They are one
//! question — does the replica count follow demand — and the shed case is the
//! interesting half, because that is where the signal has to come from somewhere
//! other than in-flight requests (ADR-0045).


use std::time::{Duration, Instant};

use comp_reconciler::fleet::Fleet;

/// Replica count over time, sampled from the lattice rather than from a log.
fn wait_for(fleet: &Fleet, want: impl Fn(u32) -> bool, within: Duration) -> (u32, Option<Duration>) {
    let start = Instant::now();
    let mut last = fleet.replicas();
    while start.elapsed() < within {
        last = fleet.replicas();
        if want(last) {
            return (last, Some(start.elapsed()));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    (last, None)
}

#[test]
fn replicas_follow_demand_and_come_back_down() {
    let fleet = Fleet::start("autoscale", &["fixtures/autoscale.yaml"], 4, None);

    // min 1, max 4, target 20 concurrent per replica.
    let (settled, _) = wait_for(&fleet, |n| n == 1, Duration::from_secs(60));
    assert_eq!(settled, 1, "should settle at min with nobody asking");

    let load = fleet.load("shop.eve.test", 120, Duration::from_secs(40));
    let (peak, at) = wait_for(&fleet, |n| n >= 3, Duration::from_secs(40));
    println!("    under load: {peak} replicas after {:?}", at);
    assert!(peak >= 3, "120 concurrent at target 20 should ask for more than {peak}");
    load.stop();

    // Scale-down waits for the cooldown — a surplus has to persist, while a deficit
    // is acted on at once (ADR-0022). So this is allowed to take longer than the way up.
    let (idle, back) = wait_for(&fleet, |n| n == 1, Duration::from_secs(60));
    println!("    idle again: {idle} replica(s) after {back:?}");
    assert_eq!(idle, 1, "should return to min once the load stops");
}

#[test]
fn shedding_grows_the_app_rather_than_hiding_the_demand() {
    // A deliberately low bound, so load hits the ingress limit rather than the
    // fleet's real capacity. Without ADR-0045 the reconciler sees a calm app carrying
    // `max_inflight` requests while the ingress refuses everything behind it, and the
    // app stays small BECAUSE it is overloaded.
    let fleet = Fleet::start("shedscale", &["fixtures/shedscale.yaml"], 4, Some(8));

    let (settled, _) = wait_for(&fleet, |n| n == 1, Duration::from_secs(60));
    assert_eq!(settled, 1);

    let load = fleet.load("shop.eve.test", 120, Duration::from_secs(45));
    let (peak, at) = wait_for(&fleet, |n| n >= 4, Duration::from_secs(45));
    println!("    while shedding: {peak} replicas after {at:?}");
    assert!(
        peak >= 4,
        "refusals are unmet demand and must grow the app; stuck at {peak}"
    );
    let (ok, shed) = load.stop();
    println!("    served {ok}, shed {shed}");
    assert!(shed > 0, "the bound was not low enough to shed anything");
    assert!(ok > 0, "it shed everything — that would be a wedged app, not a busy one");
}
