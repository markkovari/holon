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

    // `serves` is NOT convergence, and taking it for one is what made this test
    // fail under load while passing in isolation — through four wrong diagnoses.
    //
    // An ingress with an empty routing table still answers: it asks the
    // reconciler to activate the app and routes to whatever address comes back.
    // So a successful request proves an instance exists SOMEWHERE, not that the
    // fleet has been placed or that any ingress can route to it. Polling until
    // requests stop failing has the same hole, for the same reason — it was the
    // second version of this gate and it also passed while inventory was empty.
    //
    // Inventory is the only honest answer, because inventory is what routing is
    // built from. Under load, placement lagged past the ten seconds this test
    // used to allow: the ingress read `3 node(s), 0 instance(s)` for its whole
    // life, every request fell through to activation, and activation answered
    // "already placed, or nothing to start". Thirty failures, and nothing at all
    // wrong with the ingress.
    assert!(
        fleet.wait_for_placement("shop.eve.test", Duration::from_secs(180)),
        "the fleet never placed the app anywhere\n--- ingress ---\n{}\n--- reconciler ---\n{}",
        fleet.ingress_log(""),
        fleet.reconciler_log()
    );

    let b = fleet.second_ingress();
    std::thread::sleep(Duration::from_secs(6)); // let it read inventory once

    let (via_a, fail_a) = fleet.who_answers(fleet.ingress_port, 30);
    let (via_b, fail_b) = fleet.who_answers(b, 30);
    println!("    ingress A -> {via_a:?} ({fail_a} failed)");
    println!("    ingress B -> {via_b:?} ({fail_b} failed)");
    // The logs, not just the counts: this assertion has failed under a loaded
    // parallel run while passing every time in isolation, and a bare number
    // cannot distinguish an ingress that lost its lattice connection from one
    // that never read inventory from a machine that ran out of something.
    assert_eq!(
        fail_a + fail_b,
        0,
        "both ingresses should serve while both are up\n\
         --- ingress A ---\n{}\n--- ingress B ---\n{}",
        fleet.ingress_log(""),
        fleet.ingress_log("-b")
    );
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
