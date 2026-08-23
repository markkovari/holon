//! An ingress must not turn a momentary gap into an outage.
//!
//! This is the `ha.rs` failure made deterministic. That test only went wrong when
//! the machine was busy enough for the timing to slip, which is a lottery rather
//! than a test — it passed alone, failed under the full suite, and passed again
//! on a re-run, which is exactly the pattern that lets a real bug be dismissed as
//! flakiness. It was dismissed as flakiness twice, by me.
//!
//! So the gap is CAUSED here instead of waited for. The fleet stays healthy the
//! whole time; only the inventory is made to look empty, by publishing each
//! node's snapshot with no instances in it. That is precisely what the ingress
//! saw when it failed: three nodes present, none of them contributing a route.
//!
//! What must hold: the ingress keeps serving from the table it already has. A
//! blink in the control plane is not a reason to stop routing to backends that
//! are plainly still running.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use comp_lattice::{nats::NatsLattice, Inventory};
use comp_reconciler::fleet::Fleet;

/// Publish every node's key with an EMPTY instance list, over and over, so the
/// gap is held open for as long as the caller wants rather than for one refresh.
///
/// The hosts keep heartbeating their real snapshots underneath, so this is a
/// race that has to be run continuously — which is also a fair imitation of the
/// real thing, where whatever caused the gap did not stop after one tick.
fn hold_inventory_empty(
    nats_url: String,
    lattice: String,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<usize> {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let inv = NatsLattice::connect(&nats_url, &lattice, Duration::from_secs(15))
                .await
                .expect("connecting to the fleet's lattice");
            // Learn the real keys and the snapshot shape first.
            let entries = inv.read_all().await.expect("reading inventory");
            let mut blanked = 0usize;
            while !stop.load(Ordering::Relaxed) {
                for e in &entries {
                    let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(
                        &comp_lattice::snapshot::expand(e.value.clone()),
                    ) else {
                        continue;
                    };
                    // Everything else about the node stays true — it is present,
                    // it has an address, it has capacity. Only its instances are
                    // gone, which is the exact shape of the observed failure.
                    v["instances"] = serde_json::json!([]);
                    let bytes = comp_lattice::snapshot::compress(v.to_string().into_bytes());
                    if inv.publish(&e.key, bytes, Duration::from_secs(15)).await.is_ok() {
                        blanked += 1;
                    }
                }
                // Fast enough to win against a host heartbeat reliably. Losing
                // the race means the ingress never sees the gap, and a test that
                // does not create the condition proves nothing.
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            blanked
        })
    })
}

/// Hammer the ingress and count what came back.
fn drive(port: u16, host: &str, until: Instant) -> (usize, usize, Option<String>) {
    let client =
        reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).build().unwrap();
    let (mut ok, mut failed) = (0, 0);
    let mut first_failure = None;
    while Instant::now() < until {
        match client.get(format!("http://127.0.0.1:{port}/")).header("host", host).send() {
            Ok(r) if r.status().is_success() => ok += 1,
            Ok(r) => {
                failed += 1;
                if first_failure.is_none() {
                    let status = r.status();
                    let body = r.text().unwrap_or_default();
                    first_failure = Some(format!("{status}: {}", body.trim()));
                }
            }
            Err(e) => {
                failed += 1;
                if first_failure.is_none() {
                    first_failure = Some(format!("transport: {e}"));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    (ok, failed, first_failure)
}

#[test]
fn an_ingress_keeps_serving_through_an_empty_inventory() {
    let fleet = Fleet::start("blink", &["fixtures/five-replicas.yaml"], 3, None);
    assert!(fleet.serves("shop.eve.test", Duration::from_secs(120)), "never served to begin with");

    // A baseline, so a failure during the gap cannot be blamed on the fleet
    // simply never having worked.
    let (ok, failed, why) =
        drive(fleet.ingress_port, "shop.eve.test", Instant::now() + Duration::from_secs(2));
    assert_eq!(failed, 0, "the fleet was already broken before the test began: {why:?}");
    assert!(ok > 0, "no requests were made");

    // --- the gap ------------------------------------------------------------
    let stop = Arc::new(AtomicBool::new(false));
    let blanker = hold_inventory_empty(fleet.nats_url.clone(), fleet.lattice.clone(), stop.clone());

    // Long enough to cover several refreshes: the whole question is whether the
    // ingress adopts an empty table when it reads one, and it only reads every
    // few seconds.
    let (ok, failed, why) =
        drive(fleet.ingress_port, "shop.eve.test", Instant::now() + Duration::from_secs(12));
    stop.store(true, Ordering::Relaxed);
    let blanked = blanker.join().unwrap();

    assert!(blanked > 0, "the test never managed to blank the inventory, so it proved nothing");

    // THE CHECK THAT MAKES THIS TEST WORTH HAVING.
    //
    // Serving throughout only means something if the ingress actually READ an
    // empty inventory. Without this the test passes against the very bug it
    // exists to catch — because an ingress with no routes falls back to asking
    // the reconciler to activate, and that path can serve a request all by
    // itself. The first version of this test did exactly that, and was worthless.
    let log = fleet.ingress_log("");
    let saw_the_gap = log.contains("keeping the table it had")   // rode it out
        || log.contains("0 route(s) over")                        // or adopted it
        || log.contains("still 0 routes from");
    assert!(
        saw_the_gap,
        "the ingress never observed an empty inventory, so nothing here was tested. \
         {blanked} blanking write(s) went in, but the hosts won the race.\n--- ingress ---\n{log}"
    );
    assert!(
        log.contains("keeping the table it had"),
        "the ingress saw the gap and did NOT ride it out — it threw away a working \
         routing table because one read came back empty.\n--- ingress ---\n{log}"
    );
    assert!(ok + failed > 0, "no requests were made during the gap");
    assert_eq!(
        failed,
        0,
        "the ingress stopped serving while every backend was still running. {ok} ok, {failed} \
         failed, first: {why:?}\n\
         An inventory that reads empty is a gap to ride out, not a reason to throw away a \
         working routing table — this is the ha.rs failure, and it is what `verdict` exists \
         to prevent.\n--- ingress ---\n{}",
        fleet.ingress_log("")
    );

    // --- and it recovers ----------------------------------------------------
    // The hosts have been heartbeating real snapshots underneath the whole time,
    // so once the blanking stops the table must come back on its own. An ingress
    // that rode out the gap by latching would pass the assertion above and still
    // be useless.
    std::thread::sleep(Duration::from_secs(8));
    let (ok, failed, why) =
        drive(fleet.ingress_port, "shop.eve.test", Instant::now() + Duration::from_secs(3));
    assert_eq!(
        failed,
        0,
        "the ingress did not recover after the inventory came back: {ok} ok, {failed} failed, \
         first: {why:?}\n--- ingress ---\n{}",
        fleet.ingress_log("")
    );

    println!("    served throughout an empty inventory, and recovered after it");
}
