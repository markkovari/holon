//! An ingress must not turn a momentary gap into an outage.
//!
//! The `ha.rs` failure made deterministic: that test only went wrong when the machine was busy enough
//! for the timing to slip, and was dismissed as flakiness twice. So the gap is CAUSED here: the fleet
//! stays healthy and only the inventory looks empty, by publishing each node's snapshot with no
//! instances (what the ingress saw when it failed: nodes present, none contributing a route).
//!
//! What must hold: the ingress keeps serving from the table it already has.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use comp_fleet::fleet::Fleet;
use comp_lattice::nats::NatsLattice;
use comp_lattice::Inventory;

/// Publish every node's key with an EMPTY instance list, over and over, so the gap is held open as
/// long as the caller wants. The hosts keep heartbeating real snapshots underneath, so this is a race
/// run continuously.
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
                    // Everything else about the node stays true; only its instances are gone, the
                    // exact shape of the observed failure.
                    v["instances"] = serde_json::json!([]);
                    // Plain JSON: `publish` compresses, and compressing here too left bytes no reader
                    // could expand, so the gap was an unreadable inventory, not an empty one.
                    let bytes = v.to_string().into_bytes();
                    if inv.publish(&e.key, bytes, Duration::from_secs(15)).await.is_ok() {
                        blanked += 1;
                    }
                }
                // Fast enough to win against a host heartbeat; losing means the ingress never sees the gap.
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            blanked
        })
    })
}

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

    // A baseline, so a failure during the gap cannot be blamed on the fleet never having worked.
    let (ok, failed, why) =
        drive(fleet.ingress_port, "shop.eve.test", Instant::now() + Duration::from_secs(2));
    assert_eq!(failed, 0, "the fleet was already broken before the test began: {why:?}");
    assert!(ok > 0, "no requests were made");

    let stop = Arc::new(AtomicBool::new(false));
    let blanker = hold_inventory_empty(fleet.nats_url.clone(), fleet.lattice.clone(), stop.clone());

    // Long enough to cover several refreshes: the ingress only reads every few seconds.
    let (ok, failed, why) =
        drive(fleet.ingress_port, "shop.eve.test", Instant::now() + Duration::from_secs(12));
    stop.store(true, Ordering::Relaxed);
    let blanked = blanker.join().unwrap();

    assert!(blanked > 0, "the test never managed to blank the inventory, so it proved nothing");

    // THE CHECK THAT MAKES THIS TEST WORTH HAVING: serving throughout only means something if the
    // ingress actually READ an empty inventory. An ingress with no routes falls back to asking the
    // reconciler to activate, which can serve a request by itself.
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

    // --- and it recovers ---
    // Once the blanking stops the table must come back on its own; an ingress that rode out the gap by
    // latching would pass above and still be useless.
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
