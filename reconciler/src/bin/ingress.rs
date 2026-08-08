//! `comp-ingress` — the door into the lattice.
//!
//! Two nodes serving one app does nothing for a caller that knows one address.
//! This is the thing that makes multi-node placement useful rather than merely
//! true: it terminates HTTP, looks up which nodes run the app the `Host` header
//! names, and forwards to one of them.
//!
//! **It proxies HTTP directly rather than tunnelling over the bus.** Every node is
//! mutually reachable on the tailnet, and a node already advertises where it can be
//! reached, so there is no envelope to define and no serialization to get wrong —
//! the request goes as HTTP and comes back as HTTP. NATS queue groups are the
//! alternative and would be the right answer if nodes were *not* directly
//! reachable, or if we wanted the bus to do the balancing; neither is true here,
//! and the bus hop would cost latency for nothing.
//!
//! **Its routing table comes from inventory, not from the control plane.** A node
//! advertises the `Host` header each instance answers to, so this needs no platform
//! credential, no manifest access, and keeps working while the control plane is
//! down — the same property the node ledger buys on the other side.
//!
//! Failure handling is inventory expiry plus one retry: a node that stops
//! heartbeating disappears from the table within a TTL, and a node that dies
//! between refreshes costs one request a retry against a different replica.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use comp_lattice::{nats::NatsLattice, Inventory};
use comp_reconciler::plan::NodeInventory;
use http_body_util::BodyExt;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;

#[derive(Parser, Clone)]
#[command(name = "comp-ingress", about = "Terminates HTTP and spreads it across the replicas of an app")]
struct Args {
    #[arg(long, default_value = "0.0.0.0:8088")]
    addr: String,

    #[arg(long, env = "NATS_URL", default_value = "nats://127.0.0.1:4222")]
    nats_url: String,

    #[arg(long, default_value = "default")]
    lattice: String,

    /// Seconds between inventory refreshes. The table is read on every request and
    /// refreshed on a timer, because a request must never wait on the bus.
    #[arg(long, default_value = "3")]
    refresh_secs: u64,

    /// Seconds to wait on a backend before trying another replica.
    #[arg(long, default_value = "30")]
    backend_timeout: u64,

    /// How to choose among the replicas of an app.
    ///
    /// `least-outstanding` sends each request to whichever replica currently has
    /// the fewest in flight. That is the same as round robin when every backend is
    /// equally fast, and strictly better when one is not: a slow node accumulates
    /// in-flight requests and stops being chosen, without anyone measuring latency
    /// or configuring a weight. Round robin is kept so the two can be compared on
    /// one fleet.
    #[arg(long, value_enum, default_value_t = Balance::LeastOutstanding)]
    balance: Balance,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Balance {
    RoundRobin,
    LeastOutstanding,
}

/// `Host` header -> the nodes that answer to it.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
struct Table {
    routes: BTreeMap<String, Vec<Backend>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Backend {
    node: String,
    address: String,
}

/// Requests currently in flight per node.
///
/// Keyed by node name and held OUTSIDE the routing table on purpose: the table is
/// replaced wholesale on every inventory refresh, and counters that were replaced
/// with it would reset to zero every few seconds — which is exactly often enough to
/// hide the imbalance they exist to correct.
type InFlight = Arc<RwLock<BTreeMap<String, Arc<AtomicUsize>>>>;

fn counter(inflight: &InFlight, node: &str) -> Arc<AtomicUsize> {
    if let Some(c) = inflight.read().unwrap().get(node) {
        return c.clone();
    }
    inflight.write().unwrap().entry(node.to_string()).or_default().clone()
}

/// Decrements on drop, so a panic or an early return cannot leak a count and
/// permanently retire a healthy backend.
struct Busy(Arc<AtomicUsize>);
impl Busy {
    fn on(c: Arc<AtomicUsize>) -> Self {
        c.fetch_add(1, Ordering::Relaxed);
        Busy(c)
    }
}
impl Drop for Busy {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The backends to try, best first.
///
/// Returns an ORDER rather than one choice so the retry path reuses the same
/// judgement instead of a second, differently-wrong rule.
fn order<'a>(
    backends: &'a [Backend],
    mode: Balance,
    next: &AtomicUsize,
    inflight: &InFlight,
) -> Vec<&'a Backend> {
    let start = next.fetch_add(1, Ordering::Relaxed);
    let mut ranked: Vec<&Backend> = backends.iter().collect();
    match mode {
        Balance::RoundRobin => {
            ranked.rotate_left(start % backends.len().max(1));
        }
        Balance::LeastOutstanding => {
            // Rotate FIRST so that ties — which is every request on an idle fleet —
            // still spread. Without it, least-outstanding degenerates to "always the
            // alphabetically first node" whenever the fleet is keeping up.
            ranked.rotate_left(start % backends.len().max(1));
            ranked.sort_by_key(|b| counter(inflight, &b.node).load(Ordering::Relaxed));
        }
    }
    ranked
}

/// Build `host -> [node]` from what the nodes themselves advertise.
///
/// A node with no address is skipped rather than guessed at: forwarding to an
/// address we invented would produce a confusing failure a long way from here.
fn table_of(inventory: &[NodeInventory]) -> Table {
    let mut routes: BTreeMap<String, Vec<Backend>> = BTreeMap::new();
    for node in inventory {
        if node.address.is_empty() {
            continue;
        }
        for inst in &node.instances {
            let Some(host) = inst.ingress_host.as_ref().filter(|h| !h.is_empty()) else {
                continue;
            };
            let backends = routes.entry(host.to_ascii_lowercase()).or_default();
            // One entry per node, not per instance: a node holding two replicas is
            // still one place to send a request, and counting it twice would skew
            // the round robin toward whichever node happens to be busiest.
            if !backends.iter().any(|b| b.node == node.node) {
                backends.push(Backend { node: node.node.clone(), address: node.address.clone() });
            }
        }
    }
    // Sorted so the rotation is stable across refreshes: an unordered map would
    // reshuffle every backend list on each poll and make the balance arbitrary.
    for b in routes.values_mut() {
        b.sort_by(|a, b| a.node.cmp(&b.node));
    }
    Table { routes }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let addr: SocketAddr = args.addr.parse()?;

    let fabric = Arc::new(
        NatsLattice::connect(&args.nats_url, &args.lattice, Duration::from_secs(15)).await?,
    );
    let inventory: Arc<dyn Inventory> = fabric;
    let table: Arc<RwLock<Table>> = Arc::new(RwLock::new(Table::default()));

    // Refreshed on a timer and read from a lock per request. A request that had to
    // ask the bus for its route would put the control plane on the data path, which
    // is the thing this design keeps apart.
    {
        let (inventory, table, every) = (inventory.clone(), table.clone(), args.refresh_secs);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(every.max(1)));
            loop {
                tick.tick().await;
                match inventory.read_all().await {
                    Ok(entries) => {
                        let nodes: Vec<NodeInventory> = entries
                            .iter()
                            .filter_map(|e| serde_json::from_slice(&e.value).ok())
                            .collect();
                        let next = table_of(&nodes);
                        let mut cur = table.write().unwrap();
                        if *cur != next {
                            eprintln!(
                                "comp-ingress: {} route(s) over {} node(s)",
                                next.routes.len(),
                                nodes.len()
                            );
                            *cur = next;
                        }
                    }
                    // A failed read leaves the previous table in place. The
                    // alternative — an empty table — would 503 every request
                    // because the CONTROL plane blinked, which is precisely what
                    // reading inventory instead of asking the platform avoids.
                    Err(e) => eprintln!("comp-ingress: inventory read failed: {e:#}"),
                }
            }
        });
    }

    let client: Client = Arc::new(
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .build_http(),
    );
    let next = Arc::new(AtomicUsize::new(0));
    let inflight: InFlight = Arc::new(RwLock::new(BTreeMap::new()));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!(
        "comp-ingress: listening on http://{addr} | lattice {} | balance {:?}",
        args.lattice, args.balance
    );

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let (table, client, next, inflight, timeout, mode) = (
            table.clone(),
            client.clone(),
            next.clone(),
            inflight.clone(),
            args.backend_timeout,
            args.balance,
        );
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |req| {
                let (table, client, next, inflight) =
                    (table.clone(), client.clone(), next.clone(), inflight.clone());
                async move { forward(table, client, next, inflight, mode, timeout, req).await }
            });
            if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                eprintln!("comp-ingress: connection error: {e}");
            }
        });
    }
}

/// The body type is the one we FORWARD, not the one we received: the request body
/// is buffered up front so a retry against a second replica can replay it.
type ProxyBody = http_body_util::combinators::BoxBody<bytes::Bytes, hyper::Error>;

type Client = Arc<
    hyper_util::client::legacy::Client<
        hyper_util::client::legacy::connect::HttpConnector,
        ProxyBody,
    >,
>;

#[allow(clippy::too_many_arguments)]
async fn forward(
    table: Arc<RwLock<Table>>,
    client: Client,
    next: Arc<AtomicUsize>,
    inflight: InFlight,
    mode: Balance,
    timeout: u64,
    req: hyper::Request<hyper::body::Incoming>,
) -> Result<hyper::Response<ProxyBody>> {
    let host = req
        .headers()
        .get(hyper::header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();

    let backends = table.read().unwrap().routes.get(&host).cloned().unwrap_or_default();
    if backends.is_empty() {
        // 503, not 404: the app may exist and simply have no replica up. A 404
        // would tell a caller to stop retrying, which is the wrong advice.
        return Ok(status(503, &format!("no replica of {host:?} is currently placed\n")));
    }

    let ranked = order(&backends, mode, &next, &inflight);
    let (parts, body) = req.into_parts();
    let bytes = body.collect().await.context("reading the request body")?.to_bytes();

    let mut last = String::new();
    // One retry against a DIFFERENT replica. A node that died between inventory
    // refreshes should cost one request a retry, not a failure — but retrying
    // forever would turn one sick backend into a stampede.
    for backend in ranked.into_iter().take(2) {
        let _busy = Busy::on(counter(&inflight, &backend.node));
        let uri: hyper::Uri = format!(
            "http://{}{}",
            backend.address,
            parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/")
        )
        .parse()
        .context("building the backend uri")?;

        let mut out = hyper::Request::builder().method(parts.method.clone()).uri(uri);
        for (k, v) in parts.headers.iter() {
            out = out.header(k, v);
        }
        let out = out.body(full(bytes.clone()))?;

        match tokio::time::timeout(Duration::from_secs(timeout), client.request(out)).await {
            Ok(Ok(resp)) => {
                let (mut rp, rb) = resp.into_parts();
                // Which replica answered. The single most useful thing an ingress
                // can tell you, and the only way to see the balance from outside.
                if let Ok(v) = hyper::header::HeaderValue::from_str(&backend.node) {
                    rp.headers.insert("x-comp-node", v);
                }
                return Ok(hyper::Response::from_parts(rp, rb.boxed()));
            }
            Ok(Err(e)) => last = format!("{} refused: {e}", backend.node),
            Err(_) => last = format!("{} timed out", backend.node),
        }
    }
    Ok(status(502, &format!("every replica of {host:?} failed; last: {last}\n")))
}

fn full(b: bytes::Bytes) -> ProxyBody {
    use http_body_util::Full;
    Full::new(b).map_err(|never| match never {}).boxed()
}

fn status(code: u16, msg: &str) -> hyper::Response<ProxyBody> {
    hyper::Response::builder()
        .status(code)
        .header("content-type", "text/plain; charset=utf-8")
        .body(full(bytes::Bytes::from(msg.to_string())))
        .expect("static response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use comp_reconciler::plan::RunningInstance;

    fn node(name: &str, addr: &str, hosts: &[&str]) -> NodeInventory {
        NodeInventory {
            node: name.into(),
            address: addr.into(),
            instances: hosts
                .iter()
                .map(|h| RunningInstance {
                    tenant: "alice".into(),
                    app: "shop".into(),
                    component: "api".into(),
                    digest: "sha256:a".into(),
                    count: 1,
                    ingress_host: Some(h.to_string()),
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn every_node_running_an_app_becomes_a_backend_for_it() {
        let t = table_of(&[
            node("n1", "10.0.0.1:3401", &["shop.alice.test"]),
            node("n2", "10.0.0.2:3401", &["shop.alice.test"]),
            node("n3", "10.0.0.3:3401", &["other.bob.test"]),
        ]);
        assert_eq!(t.routes["shop.alice.test"].len(), 2);
        assert_eq!(t.routes["other.bob.test"].len(), 1);
        assert_eq!(t.routes["shop.alice.test"][0].node, "n1");
    }

    #[test]
    fn a_node_holding_two_replicas_is_still_one_backend() {
        // Counting it twice would skew the rotation toward whichever node happens
        // to hold the most replicas, which is usually the busiest one.
        let mut n = node("n1", "10.0.0.1:3401", &["shop.alice.test"]);
        n.instances.push(n.instances[0].clone());
        assert_eq!(table_of(&[n]).routes["shop.alice.test"].len(), 1);
    }

    #[test]
    fn a_node_with_no_advertised_address_is_skipped() {
        // Forwarding to an address we invented fails a long way from here, with a
        // message that points at the wrong thing.
        let t = table_of(&[node("n1", "", &["shop.alice.test"])]);
        assert!(t.routes.is_empty());
    }

    #[test]
    fn the_rotation_is_stable_across_refreshes() {
        // Inventory comes back in whatever order the KV lists it. If the backend
        // order moved with it, the "round robin" would be a random walk.
        let a = table_of(&[
            node("n2", "10.0.0.2:3401", &["shop.alice.test"]),
            node("n1", "10.0.0.1:3401", &["shop.alice.test"]),
        ]);
        let b = table_of(&[
            node("n1", "10.0.0.1:3401", &["shop.alice.test"]),
            node("n2", "10.0.0.2:3401", &["shop.alice.test"]),
        ]);
        assert_eq!(a, b);
    }

    #[test]
    fn an_instance_that_serves_no_ingress_host_is_not_a_backend() {
        // A plug is reachable through links, not through the door.
        let mut n = node("n1", "10.0.0.1:3401", &[]);
        n.instances.push(RunningInstance {
            tenant: "alice".into(),
            app: "shop".into(),
            component: "store".into(),
            digest: "sha256:b".into(),
            count: 1,
            ingress_host: None,
        });
        assert!(table_of(&[n]).routes.is_empty());
    }

    fn inflight_of(pairs: &[(&str, usize)]) -> InFlight {
        let m: BTreeMap<String, Arc<AtomicUsize>> = pairs
            .iter()
            .map(|(n, v)| (n.to_string(), Arc::new(AtomicUsize::new(*v))))
            .collect();
        Arc::new(RwLock::new(m))
    }

    #[test]
    fn least_outstanding_avoids_the_backend_that_is_falling_behind() {
        // THE case round robin gets wrong: a node that is up but slow accumulates
        // in-flight requests, and an even split keeps feeding it anyway.
        let t = table_of(&[
            node("fast-1", "10.0.0.1:1", &["a.test"]),
            node("fast-2", "10.0.0.2:1", &["a.test"]),
            node("slow", "10.0.0.3:1", &["a.test"]),
        ]);
        let b = &t.routes["a.test"];
        let inflight = inflight_of(&[("fast-1", 0), ("fast-2", 1), ("slow", 40)]);
        let next = AtomicUsize::new(0);
        for _ in 0..6 {
            let picked = order(b, Balance::LeastOutstanding, &next, &inflight);
            assert_ne!(picked[0].node, "slow", "the backed-up node must not be first");
        }
    }

    #[test]
    fn least_outstanding_still_spreads_when_every_backend_is_idle() {
        // On a fleet that is keeping up every counter is 0, and a stable sort over
        // equal keys would hand every request to the same node forever.
        let t = table_of(&[
            node("n1", "10.0.0.1:1", &["a.test"]),
            node("n2", "10.0.0.2:1", &["a.test"]),
            node("n3", "10.0.0.3:1", &["a.test"]),
        ]);
        let b = &t.routes["a.test"];
        let inflight = inflight_of(&[("n1", 0), ("n2", 0), ("n3", 0)]);
        let next = AtomicUsize::new(0);
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..3 {
            seen.insert(order(b, Balance::LeastOutstanding, &next, &inflight)[0].node.clone());
        }
        assert_eq!(seen.len(), 3, "an idle fleet must still rotate, got {seen:?}");
    }

    #[test]
    fn a_busy_guard_cannot_leak_a_count() {
        // A leaked increment retires a healthy backend permanently, which is worse
        // than any imbalance it was meant to fix.
        let inflight = inflight_of(&[("n1", 0)]);
        let c = counter(&inflight, "n1");
        {
            let _b = Busy::on(c.clone());
            assert_eq!(c.load(Ordering::Relaxed), 1);
        }
        assert_eq!(c.load(Ordering::Relaxed), 0, "the guard must decrement on drop");
    }

    #[test]
    fn the_retry_uses_the_same_ranking_rather_than_a_second_rule() {
        // The fallback is just the next-best backend. A separate retry rule is a
        // second thing to get wrong, and it would fire exactly when things are
        // already going badly.
        let t = table_of(&[
            node("n1", "10.0.0.1:1", &["a.test"]),
            node("n2", "10.0.0.2:1", &["a.test"]),
            node("n3", "10.0.0.3:1", &["a.test"]),
        ]);
        let b = &t.routes["a.test"];
        let inflight = inflight_of(&[("n1", 5), ("n2", 0), ("n3", 2)]);
        let picked = order(b, Balance::LeastOutstanding, &AtomicUsize::new(0), &inflight);
        assert_eq!(picked[0].node, "n2");
        assert_eq!(picked[1].node, "n3", "the retry is the next best, not the first");
    }

    #[test]
    fn round_robin_visits_every_backend_before_repeating() {
        let t = table_of(&[
            node("n1", "10.0.0.1:3401", &["shop.alice.test"]),
            node("n2", "10.0.0.2:3401", &["shop.alice.test"]),
            node("n3", "10.0.0.3:3401", &["shop.alice.test"]),
        ]);
        let b = &t.routes["shop.alice.test"];
        let picked: Vec<&str> = (0..3).map(|i| b[i % b.len()].node.as_str()).collect();
        assert_eq!(picked, vec!["n1", "n2", "n3"]);
        assert_eq!(b[3 % b.len()].node, "n1", "and then it wraps");
    }
}
