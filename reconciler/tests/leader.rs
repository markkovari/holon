//! Two reconcilers, one lattice. Exactly one acts, and when it dies the other
//! takes over.
//!
//! The reconciler was the last control component with no standby — the ingress
//! has had one since ADR-0029, and `tests/ha.rs` asserts it. If this process
//! died, the fleet kept serving whatever it already ran and silently stopped
//! adapting: no scale-up, no re-placement after a node loss, no distribution.
//!
//! Running two was not a workaround, because scale-DOWN waits for a surplus to
//! persist across `settle_passes` consecutive passes and that counter lives in
//! each process's `Hysteresis`. Two loops count separately, disagree about when
//! the cooldown elapsed, and both then issue stops.

use std::time::Duration;

use comp_reconciler::fleet::Fleet;

/// Poll a log until it says something, so the test does not sleep on a guess.
fn wait_for(mut read: impl FnMut() -> String, needle: &str, within: Duration) -> String {
    let deadline = std::time::Instant::now() + within;
    let mut last = String::new();
    while std::time::Instant::now() < deadline {
        last = read();
        if last.contains(needle) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    last
}

#[test]
fn a_second_reconciler_stands_by_and_takes_over_when_the_first_dies() {
    let mut fleet = Fleet::start("leader", &["fixtures/one-replica.yaml"], 1, None);
    assert!(fleet.serves("shop.eve.test", Duration::from_secs(90)), "the fleet never served");

    // The first one is the leader; nothing else is running yet.
    let first = wait_for(|| fleet.reconciler_log(), "is now the leader", Duration::from_secs(30));
    assert!(first.contains("is now the leader"), "the only reconciler never took the lease:\n{first}");

    // A second one must NOT reconcile. It says who it is waiting for, which is
    // the difference between a standby and a process that is merely broken.
    fleet.second_reconciler("b");
    let standby = wait_for(
        || fleet.reconciler_log_named("b"),
        "standing by",
        Duration::from_secs(30),
    );
    assert!(standby.contains("standing by"), "the second reconciler did not stand by:\n{standby}");
    assert!(
        !standby.contains("is now the leader"),
        "TWO LEADERS AT ONCE — they will fight over scale-down:\n{standby}"
    );
    // And a standby is genuinely idle: it issues no commands at all.
    assert!(
        !standby.contains("start ") && !standby.contains("stop "),
        "the standby sent commands while another process held the lease:\n{standby}"
    );

    // Kill the leader. The lease is 6s here (30s in production), so the standby
    // should take over within that plus one interval.
    fleet.kill_first_reconciler();
    let promoted = wait_for(
        || fleet.reconciler_log_named("b"),
        "is now the leader",
        Duration::from_secs(45),
    );
    assert!(
        promoted.contains("is now the leader"),
        "the standby never took over after the leader died — the fleet is now \
         unreconciled, which is exactly what this exists to prevent:\n{promoted}"
    );

    // And it is a working reconciler, not just a process holding a lease: the
    // fleet still answers, which means the app it re-derived is still placed.
    assert!(
        fleet.serves("shop.eve.test", Duration::from_secs(60)),
        "the fleet stopped serving after failover"
    );
    println!("    one leader, a standby, and a takeover");
}
