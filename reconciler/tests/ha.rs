//! Two ingresses, then one of them dies.
//!
//! The ingress holds no state beyond a cache of inventory, so several should be able
//! to run and losing one should cost nothing. "Should" is the word this exists to
//! remove — it was `bench/adversarial/ha-check.py`, whose caller was deleted in the
//! refactor, which is how a check quietly stops being run.


use std::time::Duration;

use comp_reconciler::fleet::Fleet;

#[test]
fn a_second_ingress_serves_the_same_fleet_and_outlives_the_first() {
    let mut fleet = Fleet::start("ha", &["fixtures/five-replicas.yaml"], 3, None);
    assert!(fleet.serves("shop.eve.test", Duration::from_secs(90)), "never served");

    let b = fleet.second_ingress();
    std::thread::sleep(Duration::from_secs(6)); // let it read inventory once

    let (via_a, fail_a) = fleet.who_answers(fleet.ingress_port, 30);
    let (via_b, fail_b) = fleet.who_answers(b, 30);
    println!("    ingress A -> {via_a:?} ({fail_a} failed)");
    println!("    ingress B -> {via_b:?} ({fail_b} failed)");
    assert_eq!(fail_a + fail_b, 0, "both ingresses should serve while both are up");
    assert!(!via_b.is_empty(), "the second ingress served nothing");
    // They are looking at one lattice, so they should reach the same nodes — not
    // necessarily in the same proportion, since each balances independently.
    assert!(
        via_a.keys().any(|n| via_b.contains_key(n)),
        "the two ingresses reached disjoint node sets: {via_a:?} vs {via_b:?}"
    );

    // Kill the one that was started last (B's own child), then check A still serves.
    fleet.kill_last();
    std::thread::sleep(Duration::from_secs(2));
    let (after, failed) = fleet.who_answers(fleet.ingress_port, 30);
    println!("    after killing one: {after:?} ({failed} failed)");
    assert_eq!(failed, 0, "killing one ingress cost the other one requests");
}
