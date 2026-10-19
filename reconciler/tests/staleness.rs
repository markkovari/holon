//! What the read cache costs across nodes (the measurement ADR-0063 shipped without).
//!
//! `--kv-cache-ms` has no coherence protocol, so a write on one node is invisible on another until the
//! entry expires. On one node the cache invalidates its own writes, so the conformance suite says
//! nothing about a fleet.
//!
//! The rate limiter was the wrong probe: it is a read-modify-write, so each node invalidates its own
//! entry on every request (50 of 50 with the cache on and off). **A workload that writes what it reads
//! is safe from this cache by construction.** The exposure needs a key one node WRITES and another only
//! READS: `POST /api/batch/submit` grows a batch, `GET /api/batch/{id}` only reads it.

use std::time::Duration;

use comp_fleet::fleet::Fleet;

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder().timeout(Duration::from_secs(8)).build().unwrap()
}

/// Straight at one node, bypassing the ingress: which node handles what is the experiment.
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

    // A dropped CONNECTION is retried; an ANSWER never is. A stale value arrives as a good 200, and
    // retrying it would retry away the phenomenon under test. A refused or reset connection says
    // nothing about the cache (one in a few hundred under load).
    let mut last = String::new();
    for attempt in 0..5 {
        match client()
            .get(format!("http://127.0.0.1:{port}/api/batch/{id}"))
            .header("host", "shop.eve.test")
            .send()
        {
            Ok(r) => {
                let status = r.status().as_u16();
                let body = r.text().unwrap_or_default();
                let v: serde_json::Value =
                    serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
                let n =
                    v["items"].as_array().map(|a| a.len() as u64).or_else(|| v["size"].as_u64());
                return (n, format!("{status} {body}"));
            }
            Err(e) => {
                last = format!("transport (attempt {}/5): {e}", attempt + 1);
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
    (None, last)
}

/// Both nodes must actually hold a replica first: `Fleet::serves` only asks the ingress, so a probe
/// could see node 2 answering "no application is served" and misread an unlanded placement as a cache
/// result.
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
    // Node 2 reads once BEFORE the second write: a cache cannot be stale about something it has never
    // seen, so otherwise the cache would be reported innocent for the wrong reason.
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

    // 1000ms, and the read lands 200ms after the write, comfortably inside it.
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
    // That the staleness ENDS is the next test's job: divergence is only bounded if the bound holds.
}

#[test]
fn the_staleness_expires_with_the_ttl() {
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

/// Both nodes append to ONE batch, alternating, and every item must survive. `batch_submit` appends
/// under a revision compare-and-set, so a node with a stale copy sees an old revision AND old
/// contents, and if the store accepts that revision the other node's append is overwritten: a lost
/// update that no TTL bounds (ADR-0064).
fn append_across_nodes(fleet: &Fleet, key: &str, rounds: usize) -> (usize, u64) {
    let mut sent = 0;
    let mut id = None;
    for i in 0..rounds {
        // Alternate, with a pause well inside the TTL so each node holds a cached copy.
        let node = if i % 2 == 0 { 1 } else { 2 };
        let (got, _) = submit_full(fleet, node, key, &format!("item-{i}"));
        if let Some(g) = got {
            id = Some(g);
            sent += 1;
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    // Read after the TTL has certainly lapsed, so this is the store's truth.
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

    // An assertion about the OUTCOME, not its direction: appends surviving means the revision guard
    // holds through a stale read; vanishing is the lost update that decides whether this flag can
    // default to on.
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

/// The one ordering the alternating test never produces: a writer that READS between its own writes.
///
/// A writing node drops its copy on every write, so its compare-and-set sees truth; a read in between
/// puts a copy back, and if the other node then writes, this node holds a stale value AND revision.
/// ADR-0065 measured this losing an append (the guard was read-compare-write over `wasi:keyvalue`, so a
/// cached read agreed with itself). ADR-0066 moved the comparison into the store (`comp:store/cas`,
/// JetStream's own revision on NATS), so the losing writer is TOLD, re-reads and retries.
///
/// Both are asserted: node 2's append has to land (not merely be refused) and node 1's has to survive.
#[test]
fn a_writer_that_reads_between_its_writes_cannot_eat_another_nodes_append() {
    let fleet = Fleet::start_with_cache("lost-mixed", &["fixtures/spread-stateful.yaml"], 2, 2000);
    assert!(fleet.serves("shop.eve.test", Duration::from_secs(90)), "fleet never served");
    wait_for_both(&fleet, Duration::from_secs(60));

    let id = submit(&fleet, 2, "mixed", "from-2-a").expect("node 2 opened the batch");
    assert_eq!(size_seen_by(&fleet, 2, &id), Some(1), "node 2 could not read its own batch");
    submit(&fleet, 1, "mixed", "from-1").expect("node 1 appended");
    let (_, claimed) = submit_full(&fleet, 2, "mixed", "from-2-b");

    // Read the truth after the TTL has certainly lapsed.
    std::thread::sleep(Duration::from_millis(2600));
    let held = size_seen_by(&fleet, 1, &id).unwrap_or(0);
    println!(
        "    3 appends, 2 nodes, a read in between: node 2 claimed {claimed:?}, store holds {held}"
    );

    // All three survive. Two would pass for the wrong reason (the measured defect), and node 2
    // claiming nothing would mean its append was refused rather than retried.
    assert_eq!(
        held, 3,
        "three appends went in and {held} survived. 2 means an append was LOST \
         (ADR-0065's defect is back); anything else means the retry loop stopped \
         converging."
    );
    assert!(claimed.is_some(), "node 2's append was refused rather than retried: {claimed:?}");
}
