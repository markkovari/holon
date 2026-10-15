//! Two ingresses, then one of them dies. The ingress holds no state beyond a cache of inventory, so
//! several should be able to run and losing one should cost nothing; this removes the "should".

use std::time::Duration;

use comp_reconciler::fleet::Fleet;

#[test]
fn a_second_ingress_serves_the_same_fleet_and_outlives_the_first() {
    let mut fleet = Fleet::start("ha", &["fixtures/five-replicas.yaml"], 3, None);
    assert!(fleet.serves("shop.eve.test", Duration::from_secs(90)), "never served");

    // `serves` is NOT convergence: taking it for one made this fail under load while passing alone.
    // An ingress with an empty routing table still answers (it asks the reconciler to activate the app
    // and routes to whatever address comes back), so a successful request proves an instance exists
    // SOMEWHERE, not that any ingress can route to it. Polling until requests stop failing has the same
    // hole.
    //
    // Inventory is the only honest answer, since routing is built from it. Under load placement lagged
    // past the old ten seconds: the ingress read `3 node(s), 0 instance(s)` for its whole life and every
    // request fell through to activation.
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
    // The logs, not just the counts: a bare number cannot distinguish a lost lattice connection from an
    // ingress that never read inventory from a machine that ran out of something.
    assert_eq!(
        fail_a + fail_b,
        0,
        "both ingresses should serve while both are up\n\
         --- ingress A ---\n{}\n--- ingress B ---\n{}",
        fleet.ingress_log(""),
        fleet.ingress_log("-b")
    );
    assert!(!via_b.is_empty(), "the second ingress served nothing");
    // They look at one lattice, so they reach the same nodes, though not in the same proportion.
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
