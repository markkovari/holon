//! What the read cache costs across nodes — the measurement ADR-0063 shipped
//! without.
//!
//! `--kv-cache-ms` has no coherence protocol, so a write on one node is invisible
//! on another until the entry expires. ADR-0063 says so and then proved only the
//! single-node half: on one node the cache invalidates its own writes, so
//! read-your-own-writes holds by construction and the conformance suite passing
//! was never evidence about a fleet.
//!
//! ## The probe, and the first one that did not work
//!
//! The obvious instrument was the rate limiter: two replicas must share one
//! budget (ADR-0027), so a stale read would let more requests through than the
//! limit allows. It let through exactly 50 of 50 with the cache on and off.
//!
//! That is not a broken test, it is the answer to a different question. The
//! limiter is a read-modify-write: every request writes the key it just read, so
//! each node invalidates its own entry on every request and never holds one long
//! enough to serve it stale. **A workload that writes what it reads is safe from
//! this cache by construction**, which is worth knowing and is not what ADR-0063
//! warns about.
//!
//! The exposure needs a key that one node WRITES and another only READS. Batching
//! is exactly that pair: `POST /api/batch/submit` grows a batch, `GET
//! /api/batch/{id}` only reads it.

use std::time::Duration;

use comp_reconciler::fleet::Fleet;

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder().timeout(Duration::from_secs(8)).build().unwrap()
}

/// Straight at one node, bypassing the ingress — which node handles what is the
/// whole experiment, so it cannot be left to a balancer.
fn submit(fleet: &Fleet, node: u16, key: &str, item: &str) -> Option<String> {
    submit_full(fleet, node, key, item).0
}

/// The batch id and the size the node claimed after appending.
fn submit_full(fleet: &Fleet, node: u16, key: &str, item: &str) -> (Option<String>, Option<u64>) {
    let Some(port) = fleet.host_port(node) else { return (None, None) };
    let Ok(r) = client()
        .post(format!("http://127.0.0.1:{port}/api/batch/submit"))
        .header("host", "shop.eve.test")
        .json(&serde_json::json!({ "key": key, "item": item, "max_size": 1000, "max_age_ms": 600000 }))
        .send()
    else {
        return (None, None);
    };
    let Ok(text) = r.text() else { return (None, None) };
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    (v["batch"].as_str().map(str::to_string), v["size"].as_u64())
}

/// How many items THIS node believes the batch holds.
fn size_seen_by(fleet: &Fleet, node: u16, id: &str) -> Option<u64> {
    let (n, body) = read_batch(fleet, node, id);
    if n.is_none() {
        eprintln!("    node {node} answered: {body}");
    }
    n
}

fn read_batch(fleet: &Fleet, node: u16, id: &str) -> (Option<u64>, String) {
    let Some(port) = fleet.host_port(node) else { return (None, "no such node".into()) };
    let r = match client()
        .get(format!("http://127.0.0.1:{port}/api/batch/{id}"))
        .header("host", "shop.eve.test")
        .send()
    {
        Ok(r) => r,
        Err(e) => return (None, format!("transport: {e}")),
    };
    let status = r.status().as_u16();
    let body = r.text().unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    let n = v["items"].as_array().map(|a| a.len() as u64).or_else(|| v["size"].as_u64());
    (n, format!("{status} {body}"))
}

/// Both nodes must actually hold a replica before any of this means anything.
///
/// `Fleet::serves` only asks the ingress, and one node satisfies that — so a probe
/// that starts there can find node 2 answering "no application is served at this
/// host" and read it as a cache result. It is not; it is a placement that has not
/// landed yet.
fn wait_for_both(fleet: &Fleet, within: Duration) {
    let deadline = std::time::Instant::now() + within;
    while std::time::Instant::now() < deadline {
        let ready = (1..=2).all(|n| {
            let (_, body) = read_batch(fleet, n, "placement-probe");
            !body.contains("no application is served")
        });
        if ready {
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!("both nodes never held a replica");
}

/// Write on node 1, read on node 2, and report what node 2 saw.
fn divergence(fleet: &Fleet, key: &str) -> (u64, u64) {
    let id = submit(fleet, 1, key, "first").expect("node 1 accepted the first item");
    // Node 2 reads once BEFORE the second write. Without this the key is simply
    // cold there and the next read is a miss — a cache cannot be stale about
    // something it has never seen, and a probe that skipped this would report the
    // cache innocent for the wrong reason.
    let warm = size_seen_by(fleet, 2, &id).expect("node 2 could read the batch");

    submit(fleet, 1, key, "second").expect("node 1 accepted the second item");
    std::thread::sleep(Duration::from_millis(200));
    let after = size_seen_by(fleet, 2, &id).expect("node 2 could read the batch again");
    (warm, after)
}

#[test]
fn a_node_serves_a_stale_read_of_another_nodes_write() {
    let plain = Fleet::start("stale-off", &["fixtures/spread-stateful.yaml"], 2, None);
    assert!(plain.serves("shop.eve.test", Duration::from_secs(90)), "uncached fleet never served");
    wait_for_both(&plain, Duration::from_secs(60));
    let (warm, after) = divergence(&plain, "batch-off");
    println!("    no cache:            node 2 saw {warm}, then {after} (truth: 1, then 2)");
    assert_eq!(after, 2, "without a cache node 2 must see node 1's second write");
    drop(plain);

    // 1000ms, and the read lands 200ms after the write — comfortably inside it.
    let cached = Fleet::start_with_cache("stale-on", &["fixtures/spread-stateful.yaml"], 2, 1000);
    assert!(cached.serves("shop.eve.test", Duration::from_secs(90)), "cached fleet never served");
    wait_for_both(&cached, Duration::from_secs(60));
    let (warm_c, after_c) = divergence(&cached, "batch-on");
    println!("    --kv-cache-ms 1000:  node 2 saw {warm_c}, then {after_c} (truth: 1, then 2)");

    assert_eq!(
        after_c, 1,
        "node 2 served a FRESH read inside the TTL, so either something now \
         invalidates across nodes or this probe stopped reaching the cache: \
         expected the stale 1, got {after_c}"
    );
    // That the staleness ENDS is the other half of the claim, and it is the next
    // test's job — bounded divergence is only bounded if the bound holds.
}

#[test]
fn the_staleness_expires_with_the_ttl() {
    // The other half of the claim. Bounded divergence is only bounded if it ends.
    let fleet = Fleet::start_with_cache("stale-ttl", &["fixtures/spread-stateful.yaml"], 2, 1000);
    assert!(fleet.serves("shop.eve.test", Duration::from_secs(90)), "fleet never served");
    wait_for_both(&fleet, Duration::from_secs(60));

    let id = submit(&fleet, 1, "batch-ttl", "first").expect("node 1 accepted the first item");
    assert_eq!(size_seen_by(&fleet, 2, &id), Some(1), "node 2 could not read the batch");
    submit(&fleet, 1, "batch-ttl", "second").expect("node 1 accepted the second item");

    std::thread::sleep(Duration::from_millis(200));
    let stale = size_seen_by(&fleet, 2, &id).expect("node 2 read");
    std::thread::sleep(Duration::from_millis(1500));
    let fresh = size_seen_by(&fleet, 2, &id).expect("node 2 read again");

    println!("    inside the TTL: {stale}    after it: {fresh}    (truth: 2)");
    assert_eq!(stale, 1, "expected a stale read inside the TTL");
    assert_eq!(fresh, 2, "the staleness outlived the TTL, so the bound is not a bound");
}

/// Both nodes append to ONE batch, alternating, and every item must survive.
///
/// The shape ADR-0064 could not reach. `batch_submit` appends under a revision
/// compare-and-set, so the guard is only as good as the read it compares against:
/// a node holding a stale copy sees an old revision AND old contents, and if the
/// store accepts that revision the append is written over whatever the other node
/// added in between. That is a lost update, not a stale read, and no TTL bounds it
/// — the item is simply gone.
fn append_across_nodes(fleet: &Fleet, key: &str, rounds: usize) -> (usize, u64) {
    let mut sent = 0;
    let mut id = None;
    for i in 0..rounds {
        // Alternate, with a pause well inside the TTL so each node has a cached
        // copy of the batch it is about to modify.
        let node = if i % 2 == 0 { 1 } else { 2 };
        let (got, _) = submit_full(fleet, node, key, &format!("item-{i}"));
        if let Some(g) = got {
            id = Some(g);
            sent += 1;
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    // Read after the TTL has certainly lapsed, so what comes back is the truth in
    // the store rather than either node's copy of it.
    std::thread::sleep(Duration::from_millis(1600));
    let id = id.expect("no submit succeeded");
    let held = size_seen_by(fleet, 1, &id).unwrap_or(0);
    (sent, held)
}

#[test]
fn two_nodes_writing_one_key_do_not_lose_an_append() {
    const ROUNDS: usize = 12;

    let plain = Fleet::start("lost-off", &["fixtures/spread-stateful.yaml"], 2, None);
    assert!(plain.serves("shop.eve.test", Duration::from_secs(90)), "uncached fleet never served");
    wait_for_both(&plain, Duration::from_secs(60));
    let (sent, held) = append_across_nodes(&plain, "append-off", ROUNDS);
    println!("    no cache:            {sent} accepted, {held} survived");
    assert_eq!(held as usize, sent, "the control lost an append with no cache in play");
    drop(plain);

    let cached = Fleet::start_with_cache("lost-on", &["fixtures/spread-stateful.yaml"], 2, 1000);
    assert!(cached.serves("shop.eve.test", Duration::from_secs(90)), "cached fleet never served");
    wait_for_both(&cached, Duration::from_secs(60));
    let (sent_c, held_c) = append_across_nodes(&cached, "append-on", ROUNDS);
    println!("    --kv-cache-ms 1000:  {sent_c} accepted, {held_c} survived");

    // Deliberately an assertion about the OUTCOME, not about which way it goes.
    // Either result is publishable: appends surviving means the revision guard
    // holds through a stale read, and appends vanishing is the lost update that
    // decides whether this flag can ever default to on.
    if (held_c as usize) < sent_c {
        println!(
            "    LOST {} of {} appends — the revision guard does not survive a stale read",
            sent_c - held_c as usize,
            sent_c
        );
    } else {
        println!("    no appends lost — the revision guard held through the cache");
    }
    assert!(held_c > 0, "the cached fleet lost everything, which is a broken probe, not a finding");
}

/// The one ordering the alternating test never produces: a writer that READS
/// between its own writes.
///
/// Self-invalidation is what makes a writing node safe — it drops its copy on
/// every write, so its next read is a miss and the compare-and-set compares
/// against truth. A read in between puts a copy back. If the other node then
/// writes, this node holds a stale value AND a stale revision at the moment it
/// tries to append.
///
/// It used to be the third outcome. ADR-0065 measured this losing an append:
/// `record-store::update` enforced its revision guard as a read-compare-write over
/// `wasi:keyvalue`, so a cached read made the guard agree with itself about state
/// that was already gone, and the write clobbered it. Three appends, two survivors.
///
/// ADR-0066 moved the comparison into the store — `comp:store/cas`, backed by
/// JetStream's own revision on NATS — so the losing writer is now TOLD it lost,
/// re-reads, and retries. Three appends, three survivors.
///
/// Both halves matter and both are asserted below: node 2's append has to land
/// (it is not enough to merely refuse it) and node 1's has to survive.
#[test]
fn a_writer_that_reads_between_its_writes_cannot_eat_another_nodes_append() {
    let fleet = Fleet::start_with_cache("lost-mixed", &["fixtures/spread-stateful.yaml"], 2, 2000);
    assert!(fleet.serves("shop.eve.test", Duration::from_secs(90)), "fleet never served");
    wait_for_both(&fleet, Duration::from_secs(60));

    // 1. node 2 opens the batch, so it is the writer under test.
    let id = submit(&fleet, 2, "mixed", "from-2-a").expect("node 2 opened the batch");
    // 2. node 2 READS it — putting a copy back that its own write had dropped.
    assert_eq!(size_seen_by(&fleet, 2, &id), Some(1), "node 2 could not read its own batch");
    // 3. node 1 appends. Node 2's cached copy is now stale, well inside the 2s TTL.
    submit(&fleet, 1, "mixed", "from-1").expect("node 1 appended");
    // 4. node 2 appends from that stale copy.
    let (_, claimed) = submit_full(&fleet, 2, "mixed", "from-2-b");

    // Read the truth after the TTL has certainly lapsed.
    std::thread::sleep(Duration::from_millis(2600));
    let held = size_seen_by(&fleet, 1, &id).unwrap_or(0);
    println!(
        "    3 appends, 2 nodes, a read in between: node 2 claimed {claimed:?}, store holds {held}"
    );

    // All three survive. Two lets this pass for the wrong reason — that was the
    // measured defect — and node 2 claiming nothing would mean its append was
    // refused rather than retried, which is the halfway state a store-native CAS
    // alone leaves you in until the cache also refreshes on the guarded read.
    assert_eq!(
        held, 3,
        "three appends went in and {held} survived. 2 means an append was LOST \
         (ADR-0065's defect is back); anything else means the retry loop stopped \
         converging."
    );
    assert!(claimed.is_some(), "node 2's append was refused rather than retried: {claimed:?}");
}
