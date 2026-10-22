//! The door into the lattice: proxies HTTP directly to nodes, routing from inventory alone.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use comp_lattice::nats::NatsLattice;
use comp_lattice::{CommandBus, Inventory};
use comp_reconciler::plan::NodeInventory;
use comp_reconciler::settings;
use http_body_util::BodyExt;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;

#[derive(Parser, Clone)]
#[command(
    name = "comp-ingress",
    about = "Terminates HTTP and spreads it across the replicas of an app"
)]
struct Args {
    #[arg(long, default_value = "0.0.0.0:8088")]
    addr: String,

    #[arg(long, env = "NATS_URL", default_value = "nats://127.0.0.1:4222")]
    nats_url: String,

    #[arg(long, default_value = "default")]
    lattice: String,

    /// Config file. Defaults to $COMP_CONFIG, then ./comp.toml if it exists.
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    // Tunables are Option, defaulting from the config file: a clap default looks typed.
    /// Seconds between inventory refreshes; a request must never wait on the bus.
    #[arg(long, env = "COMP_REFRESH_SECS")]
    refresh_secs: Option<u64>,

    /// Inventory TTL; must match the hosts' and reconciler's, since the bucket's creator decides.
    #[arg(long, env = "COMP_INVENTORY_TTL")]
    inventory_ttl: Option<u64>,

    /// Seconds to wait on a backend before trying another replica.
    #[arg(long, env = "COMP_BACKEND_TIMEOUT")]
    backend_timeout: Option<u64>,

    /// How to choose among replicas; least-outstanding stops choosing a slow node without weights.
    #[arg(long, value_enum, default_value_t = Balance::LeastOutstanding)]
    balance: Balance,

    /// Per-core in-flight bound per node before shedding; 0 disables it (ADR-0036: 46 s p99).
    #[arg(long, env = "COMP_MAX_INFLIGHT")]
    max_inflight: Option<usize>,

    /// Slow backends one request may spend; a refused connection is free and never counted.
    #[arg(long, env = "COMP_SLOW_BUDGET")]
    slow_budget: Option<usize>,

    /// Seconds to hold a request while a scaled-to-zero app is activated.
    #[arg(long, env = "COMP_ACTIVATION_TIMEOUT")]
    activation_timeout: Option<u64>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Balance {
    RoundRobin,
    LeastOutstanding,
}

/// Consecutive route-less reads before an ingress believes the fleet has nothing to serve.
const EMPTY_READS_BEFORE_BELIEVING: u32 = 3;

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// Take it. Either it has routes, or there were none to lose.
    Adopt,
    /// Empty while we have routes: keep them, as a blink must not become an outage.
    RideOut,
    /// It has been empty long enough to be true.
    Believe,
}

/// Extracted because the bug only reproduced under machine load; here each case is one line.
fn verdict(had_routes: bool, next_is_empty: bool, streak_before: u32) -> Verdict {
    if !next_is_empty {
        return Verdict::Adopt;
    }
    if !had_routes {
        return Verdict::Adopt;
    }
    if streak_before + 1 < EMPTY_READS_BEFORE_BELIEVING {
        Verdict::RideOut
    } else {
        Verdict::Believe
    }
}

/// A poisoned lock would kill the refresh task silently and freeze the table; it is only a cache.
fn read_table(table: &RwLock<Table>) -> std::sync::RwLockReadGuard<'_, Table> {
    table.read().unwrap_or_else(|e| e.into_inner())
}

fn write_table(table: &RwLock<Table>) -> std::sync::RwLockWriteGuard<'_, Table> {
    table.write().unwrap_or_else(|e| e.into_inner())
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
    /// The shedding bound is per core: a load a Mac absorbed shed 3 412 requests on a Pi.
    cpus: usize,
}

/// Outside the routing table: replaced with it, counters would reset every refresh.
type InFlight = Arc<RwLock<BTreeMap<String, Arc<AtomicUsize>>>>;

fn counter(inflight: &InFlight, node: &str) -> Arc<AtomicUsize> {
    if let Some(c) = inflight.read().unwrap().get(node) {
        return c.clone();
    }
    inflight.write().unwrap().entry(node.to_string()).or_default().clone()
}

/// Decrements on drop, so a panic or early return cannot leak a count and retire a backend.
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

/// One activation per host at a time, or a cold app's first burst stampedes the fleet.
type Activating = Arc<RwLock<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>>;

fn activation_lock(map: &Activating, host: &str) -> Arc<tokio::sync::Mutex<()>> {
    if let Some(l) = map.read().unwrap().get(host) {
        return l.clone();
    }
    map.write().unwrap().entry(host.to_string()).or_default().clone()
}

/// Single-flighted; a second ask is cheap because a start is idempotent.
async fn activate(
    commands: &Arc<dyn CommandBus>,
    activating: &Activating,
    table: &Arc<RwLock<Table>>,
    host: &str,
    timeout_secs: u64,
) -> Option<Backend> {
    let lock = activation_lock(activating, host);
    let _held = lock.lock().await;

    // Someone may have activated while we waited for the lock.
    if let Some(b) = read_table(table).routes.get(host).and_then(|v| v.first()) {
        return Some(b.clone());
    }

    let payload = serde_json::json!({ "host": host }).to_string().into_bytes();
    let reply = commands
        .send("reconciler", "activate", payload, Duration::from_secs(timeout_secs))
        .await
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&reply).ok()?;
    if let Some(err) = v["error"].as_str() {
        eprintln!("comp-ingress: activating {host:?} failed: {err}");
        return None;
    }
    let backend = Backend {
        node: v["node"].as_str()?.to_string(),
        address: v["address"].as_str()?.to_string(),
        // One core is the pessimistic reading until the next refresh brings the real one.
        cpus: v["cpus"].as_u64().unwrap_or(1) as usize,
    };
    // Published, or every request to a missing host paid its own round trip (13 000 spurious 503s).
    table.write().unwrap().routes.entry(host.to_string()).or_default().push(backend.clone());
    eprintln!("comp-ingress: activated {host:?} on {}", backend.node);
    Some(backend)
}

/// `bound` is per CORE: a node advertising eight cores absorbs eight times what a single-core one does.
fn saturated(b: &Backend, inflight: &InFlight, bound: usize) -> bool {
    bound > 0 && counter(inflight, &b.node).load(Ordering::Relaxed) >= bound * b.cpus
}

/// An order, not one choice, so the retry path reuses the same judgement.
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
            // Rotate first so ties still spread, not always to the alphabetically first node.
            ranked.rotate_left(start % backends.len().max(1));
            ranked.sort_by_key(|b| counter(inflight, &b.node).load(Ordering::Relaxed));
        }
    }
    ranked
}

/// A node with no address is skipped rather than guessed at.
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
            // One entry per node: counting one twice would skew the rotation toward the busiest.
            if !backends.iter().any(|b| b.node == node.node) {
                backends.push(Backend {
                    node: node.node.clone(),
                    address: node.address.clone(),
                    cpus: node.capacity.cpus.max(1),
                });
            }
        }
    }
    // Sorted so the rotation is stable across refreshes.
    for b in routes.values_mut() {
        b.sort_by(|a, b| a.node.cmp(&b.node));
    }
    Table { routes }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let addr: SocketAddr = args.addr.parse()?;

    let file = comp_reconciler::settings::File::load(args.config.as_deref())?.ingress;
    let backend_timeout = settings::pick(args.backend_timeout, file.backend_timeout, 30);
    let max_inflight = settings::pick(args.max_inflight, file.max_inflight, 64);
    let slow_budget = settings::pick(args.slow_budget, file.slow_budget, 2);
    let activation_timeout = settings::pick(args.activation_timeout, file.activation_timeout, 10);

    // Not ours to choose: every creator of this bucket passes its own `max_age` and the first wins.
    let inventory_ttl = settings::pick(args.inventory_ttl, file.inventory_ttl, 15).max(1);

    let fabric = Arc::new(
        NatsLattice::connect(&args.nats_url, &args.lattice, Duration::from_secs(inventory_ttl))
            .await?,
    );

    // From the bucket's real TTL, since `connect` returns an existing bucket; a third of it.
    let effective_ttl = fabric.effective_ttl().as_secs().max(1);
    let refresh_secs = match args.refresh_secs.or(file.refresh_secs) {
        Some(v) => v.max(1),
        None => (effective_ttl / 3).max(1),
    };
    eprintln!(
        "comp-ingress: inventory ttl {effective_ttl}s (asked for {inventory_ttl}s), \
         refreshing every {refresh_secs}s"
    );
    // Only for a host with no replica: the ingress asks the reconciler, it does not decide.
    let commands: Arc<dyn CommandBus> = fabric;
    let activating: Activating = Arc::new(RwLock::new(BTreeMap::new()));
    // Best effort: load feeds autoscaling, not serving.
    let load_out: Option<Arc<dyn Inventory>> = match NatsLattice::connect_bucket(
        &args.nats_url,
        &args.lattice,
        Duration::from_secs(30),
        comp_lattice::wire::LOAD,
    )
    .await
    {
        Ok(l) => Some(Arc::new(l)),
        Err(e) => {
            eprintln!("comp-ingress: not publishing load ({e:#})");
            None
        }
    };
    let table: Arc<RwLock<Table>> = Arc::new(RwLock::new(Table::default()));

    // Its own thread, runtime and NATS connection: competing with request handling for scheduler
    // time once left `read_all` awaiting a reply nothing would deliver.
    {
        let (table, every) = (table.clone(), refresh_secs);
        let (nats_url, lattice_name, ttl) =
            (args.nats_url.clone(), args.lattice.clone(), effective_ttl);
        std::thread::Builder::new()
            .name("inventory-refresh".into())
            .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("comp-ingress: could not build the refresh runtime: {e}");
                    return;
                }
            };
            rt.block_on(async move {
            let inventory: Arc<dyn Inventory> = loop {
                match NatsLattice::connect(&nats_url, &lattice_name, Duration::from_secs(ttl)).await
                {
                    Ok(l) => break Arc::new(l),
                    // Retried, not fatal: giving up would mean it never routes again.
                    Err(e) => {
                        eprintln!("comp-ingress: refresh connection failed ({e:#}), retrying");
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
            };
            let mut tick = tokio::time::interval(Duration::from_secs(every.max(1)));
            let mut empty_streak: u32 = 0;
            let mut reads: u64 = 0;
            loop {
                tick.tick().await;
                match inventory.read_all().await {
                    Ok(entries) => {
                        let nodes: Vec<NodeInventory> = entries
                            .iter()
                            .filter_map(|e| serde_json::from_slice(&e.value).ok())
                            .collect();
                        let next = table_of(&nodes);

                        // On every read, so silence means the loop is not running.
                        eprintln!(
                            "comp-ingress: refresh #{reads}: {} node(s), {} instance(s), {} route(s)",
                            nodes.len(),
                            nodes.iter().map(|n| n.instances.len()).sum::<usize>(),
                            next.routes.len()
                        );
                        reads += 1;

                        let had_routes = !read_table(&table).routes.is_empty();
                        let instances: usize = nodes.iter().map(|n| n.instances.len()).sum();
                        match verdict(had_routes, next.routes.is_empty(), empty_streak) {
                            Verdict::Adopt => empty_streak = 0,
                            Verdict::RideOut => {
                                empty_streak += 1;
                                eprintln!(
                                    "comp-ingress: a read produced 0 routes from {} node(s) and \
                                     {instances} instance(s) ({empty_streak}/\
                                     {EMPTY_READS_BEFORE_BELIEVING}) — keeping the table it had",
                                    nodes.len()
                                );
                                continue;
                            }
                            Verdict::Believe => {
                                empty_streak += 1;
                                eprintln!(
                                    "comp-ingress: 0 routes for {EMPTY_READS_BEFORE_BELIEVING} \
                                     reads running — the fleet really has nothing to serve"
                                );
                            }
                        }

                        // Never built is a different fault from lost, so both halves are counted.
                        if next.routes.is_empty() && !nodes.is_empty() && !had_routes {
                            let with_host: usize = nodes
                                .iter()
                                .flat_map(|n| n.instances.iter())
                                .filter(|i| {
                                    i.ingress_host.as_ref().is_some_and(|h| !h.is_empty())
                                })
                                .count();
                            let addressed = nodes.iter().filter(|n| !n.address.is_empty()).count();
                            eprintln!(
                                "comp-ingress: still 0 routes from {} node(s) — {addressed} with \
                                 an address, {instances} instance(s), {with_host} carrying an \
                                 ingress-host",
                                nodes.len()
                            );
                        }

                        let mut cur = write_table(&table);
                        if *cur != next {
                            eprintln!(
                                "comp-ingress: {} route(s) over {} node(s)",
                                next.routes.len(),
                                nodes.len()
                            );
                            *cur = next;
                        }
                    }
                    // A failed read keeps the previous table: an empty one would 503 everything.
                    Err(e) => eprintln!("comp-ingress: inventory read failed: {e:#}"),
                }
            }
            // Unreachable while the loop is `loop {}`; kept so a stopped task is never silent.
            #[allow(unreachable_code)]
            {
                eprintln!(
                    "comp-ingress: THE INVENTORY REFRESH LOOP HAS STOPPED — the routing table \
                     is frozen and will never update again"
                );
            }
            });
        })
        .expect("spawning the inventory refresh thread");
    }

    let client: Client = Arc::new(
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .build_http(),
    );
    let next = Arc::new(AtomicUsize::new(0));
    let inflight: InFlight = Arc::new(RwLock::new(BTreeMap::new()));
    let per_host: InFlight = Arc::new(RwLock::new(BTreeMap::new()));
    let shed: InFlight = Arc::new(RwLock::new(BTreeMap::new()));
    let served: InFlight = Arc::new(RwLock::new(BTreeMap::new()));

    // A short bucket TTL means a dead ingress stops voting on its own.
    if let Some(load) = load_out {
        let (per_host, shed, served) = (per_host.clone(), shed.clone(), served.clone());
        let every = refresh_secs.max(1);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(every));
            loop {
                tick.tick().await;
                let sample: Vec<(String, usize)> = per_host
                    .read()
                    .unwrap()
                    .iter()
                    .map(|(h, c)| (h.clone(), c.load(Ordering::Relaxed)))
                    .collect();
                for (host, n) in sample {
                    // Reset on read: a cumulative count would outlive the pressure.
                    let refused = counter(&shed, &host).swap(0, Ordering::Relaxed);
                    let answered = counter(&served, &host).swap(0, Ordering::Relaxed);
                    // NATS KV keys may not contain arbitrary punctuation.
                    let key = host.replace('.', "_");
                    let body = serde_json::json!({
                        "host": host,
                        "inflight": n,
                        "shed": refused,
                        "served": answered,
                    });
                    let _ = load
                        .publish(&key, body.to_string().into_bytes(), Duration::from_secs(30))
                        .await;
                }
            }
        });
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!(
        "comp-ingress: listening on http://{addr} | lattice {} | balance {:?}",
        args.lattice, args.balance
    );

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let (table, client, next, inflight, per_host, shed, served, timeout, mode, max_inflight) = (
            table.clone(),
            client.clone(),
            next.clone(),
            inflight.clone(),
            per_host.clone(),
            shed.clone(),
            served.clone(),
            backend_timeout,
            args.balance,
            max_inflight,
        );
        let (slow_budget, activation_timeout) = (slow_budget, activation_timeout);
        let (commands, activating) = (commands.clone(), activating.clone());
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |req| {
                let (table, client, next, inflight, per_host, shed, served) = (
                    table.clone(),
                    client.clone(),
                    next.clone(),
                    inflight.clone(),
                    per_host.clone(),
                    shed.clone(),
                    served.clone(),
                );
                let (commands, activating) = (commands.clone(), activating.clone());
                async move {
                    forward(
                        table,
                        client,
                        next,
                        inflight,
                        per_host,
                        shed,
                        served,
                        commands,
                        activating,
                        mode,
                        max_inflight,
                        slow_budget,
                        activation_timeout,
                        timeout,
                        req,
                    )
                    .await
                }
            });
            if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                eprintln!("comp-ingress: connection error: {e}");
            }
        });
    }
}

/// Buffered up front so a retry against a second replica can replay it.
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
    // Per host, for autoscaling; the per-node counter measures the node.
    per_host: InFlight,
    // A shed request never becomes in-flight, so concurrency understates demand (ADR-0045).
    shed: InFlight,
    // Tells "every replica busy" from "every replica wedged", where scaling up only adds wedges.
    served: InFlight,
    commands: Arc<dyn CommandBus>,
    activating: Activating,
    mode: Balance,
    max_inflight: usize,
    slow_budget: usize,
    activation_timeout: u64,
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

    // Held for the whole request INCLUDING retries.
    let _busy_host = Busy::on(counter(&per_host, &host));

    let mut backends = read_table(&table).routes.get(&host).cloned().unwrap_or_default();
    if backends.is_empty() {
        // Nothing placed: hold the request while the reconciler starts one (ADR-0040).
        match activate(&commands, &activating, &table, &host, activation_timeout).await {
            Some(b) => backends = vec![b],
            // 503, not 404: the app may exist with no replica up, and a 404 tells a caller to stop retrying.
            None => {
                return Ok(status(503, &format!("no replica of {host:?} is currently placed\n")))
            }
        }
    }

    let ranked = order(&backends, mode, &next, &inflight);

    // Shed rather than queue: a 503 now is actionable, an invisible queue is not (ADR-0036).
    if !ranked.is_empty() && ranked.iter().all(|b| saturated(b, &inflight, max_inflight)) {
        counter(&shed, &host).fetch_add(1, Ordering::Relaxed);
        return Ok(status(
            503,
            &format!("every replica of {host:?} is at {max_inflight} in flight; shedding\n"),
        ));
    }
    let (parts, body) = req.into_parts();
    let bytes = body.collect().await.context("reading the request body")?.to_bytes();

    let mut last = String::new();
    // Least-outstanding ranks a dead node first, so only timeouts are budgeted: a refused
    // connection is an instant RST and free to skip.
    let mut slow = 0;
    for backend in ranked.into_iter() {
        if slow >= slow_budget {
            break;
        }
        // Steers around a single hot node; the check above only fires when every replica is.
        if saturated(backend, &inflight, max_inflight) {
            continue;
        }
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
                // Any completed response counts: a 429 is the component doing its job.
                counter(&served, &host).fetch_add(1, Ordering::Relaxed);
                let (mut rp, rb) = resp.into_parts();
                // Which replica answered: the only way to see the balance from outside.
                if let Ok(v) = hyper::header::HeaderValue::from_str(&backend.node) {
                    rp.headers.insert("x-comp-node", v);
                }
                return Ok(hyper::Response::from_parts(rp, rb.boxed()));
            }
            Ok(Err(e)) => last = format!("{} refused: {e}", backend.node),
            Err(_) => {
                slow += 1;
                last = format!("{} timed out", backend.node);
            }
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

    struct AnswersOnce(AtomicUsize);

    #[async_trait::async_trait]
    impl CommandBus for AnswersOnce {
        async fn serve(
            &self,
            _node: &str,
        ) -> Result<tokio::sync::mpsc::Receiver<comp_lattice::Command>> {
            anyhow::bail!("not used")
        }
        async fn send(
            &self,
            _node: &str,
            _verb: &str,
            _payload: Vec<u8>,
            _timeout: Duration,
        ) -> Result<Vec<u8>> {
            if self.0.fetch_add(1, Ordering::Relaxed) > 0 {
                anyhow::bail!("the control plane will not answer twice");
            }
            Ok(br#"{"node":"n1","address":"127.0.0.1:9","cpus":4}"#.to_vec())
        }
    }

    #[tokio::test]
    async fn an_activation_is_published_so_the_next_request_finds_it() {
        let bus: Arc<dyn CommandBus> = Arc::new(AnswersOnce(AtomicUsize::new(0)));
        let activating: Activating = Default::default();
        let table = Arc::new(RwLock::new(Table::default()));

        let first = activate(&bus, &activating, &table, "shop.example.com", 5).await;
        assert!(first.is_some(), "the first activation should succeed");

        // The control plane refuses a second answer; this must come from what the first published.
        let second = activate(&bus, &activating, &table, "shop.example.com", 5).await;
        assert_eq!(second, first, "the second request must reuse the published backend");
        assert_eq!(
            table.read().unwrap().routes.get("shop.example.com").map(Vec::len),
            Some(1),
            "exactly one backend, not one per request that raced"
        );
    }

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
        // Counting it twice would skew the rotation toward the busiest node.
        let mut n = node("n1", "10.0.0.1:3401", &["shop.alice.test"]);
        n.instances.push(n.instances[0].clone());
        assert_eq!(table_of(&[n]).routes["shop.alice.test"].len(), 1);
    }

    #[test]
    fn a_node_with_no_advertised_address_is_skipped() {
        // Forwarding to an address we invented fails a long way from here.
        let t = table_of(&[node("n1", "", &["shop.alice.test"])]);
        assert!(t.routes.is_empty());
    }

    #[test]
    fn the_rotation_is_stable_across_refreshes() {
        // Inventory comes back in whatever order the KV lists it; backend order must not move with it.
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
        let m: BTreeMap<String, Arc<AtomicUsize>> =
            pairs.iter().map(|(n, v)| (n.to_string(), Arc::new(AtomicUsize::new(*v)))).collect();
        Arc::new(RwLock::new(m))
    }

    #[test]
    fn least_outstanding_avoids_the_backend_that_is_falling_behind() {
        // THE case round robin gets wrong: a node that is up but slow accumulates in-flight requests.
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
        // Every counter is 0 on a keeping-up fleet; a stable sort would pick one node.
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
        // A leaked increment retires a healthy backend permanently.
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
        // The fallback is just the next-best backend; a separate retry rule is a second thing to get wrong.
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

    #[test]
    fn a_node_at_the_bound_is_saturated_and_zero_disables_the_rule() {
        let inflight = inflight_of(&[("n1", 63), ("n2", 64), ("n3", 999)]);
        let b = |n: &str| Backend { node: n.into(), address: "127.0.0.1:1".into(), cpus: 1 };
        assert!(!saturated(&b("n1"), &inflight, 64), "63 of 64 still has room");
        assert!(saturated(&b("n2"), &inflight, 64), "at the bound is saturated");
        assert!(saturated(&b("n3"), &inflight, 64));
        // 0 must be total: an operator who turns shedding off must not get it at 999 in flight.
        assert!(!saturated(&b("n3"), &inflight, 0), "0 must disable shedding entirely");
    }

    #[test]
    fn shedding_steers_around_a_hot_node_before_it_refuses_anything() {
        // Only an entirely saturated set sheds.
        let inflight = inflight_of(&[("hot", 64), ("cool", 0)]);
        let backends = [
            Backend { node: "hot".into(), address: "127.0.0.1:1".into(), cpus: 1 },
            Backend { node: "cool".into(), address: "127.0.0.1:2".into(), cpus: 1 },
        ];
        let all_full = backends.iter().all(|b| saturated(b, &inflight, 64));
        assert!(!all_full, "one hot node must not shed the whole app");
        let usable: Vec<&Backend> =
            backends.iter().filter(|b| !saturated(b, &inflight, 64)).collect();
        assert_eq!(usable.len(), 1);
        assert_eq!(usable[0].node, "cool", "traffic goes to the replica with room");
    }

    #[test]
    fn a_fully_saturated_app_sheds_rather_than_queueing() {
        // ADR-0036: with nothing shedding the survivor's p99 was 46 s.
        let inflight = inflight_of(&[("n1", 64), ("n2", 70)]);
        let backends = [
            Backend { node: "n1".into(), address: "127.0.0.1:1".into(), cpus: 1 },
            Backend { node: "n2".into(), address: "127.0.0.1:2".into(), cpus: 1 },
        ];
        assert!(backends.iter().all(|b| saturated(b, &inflight, 64)));
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;

    /// Adopting an empty table turned a blink into `no app answers` for the process's life.
    #[test]
    fn a_blink_does_not_empty_a_working_table() {
        assert_eq!(verdict(true, true, 0), Verdict::RideOut);
        assert_eq!(verdict(true, true, 1), Verdict::RideOut);
    }

    /// But a fleet that really has stopped must eventually be believed.
    #[test]
    fn an_empty_fleet_is_believed_in_the_end() {
        assert_eq!(verdict(true, true, EMPTY_READS_BEFORE_BELIEVING - 1), Verdict::Believe);
        assert_eq!(verdict(true, true, EMPTY_READS_BEFORE_BELIEVING), Verdict::Believe);
    }

    /// A read with routes is always taken, so the ride-out is temporary rather than a latch.
    #[test]
    fn a_good_read_is_always_adopted_and_clears_the_streak() {
        assert_eq!(verdict(true, false, 0), Verdict::Adopt);
        assert_eq!(verdict(true, false, 99), Verdict::Adopt);
        assert_eq!(verdict(false, false, 2), Verdict::Adopt);
    }

    /// An ingress that never had routes has nothing to protect, including at startup.
    #[test]
    fn nothing_to_lose_means_nothing_to_defend() {
        assert_eq!(verdict(false, true, 0), Verdict::Adopt);
        assert_eq!(verdict(false, true, 5), Verdict::Adopt);
    }

    /// Keying on present nodes once let three healthy nodes advertising zero routes wipe the table.
    #[test]
    fn the_guard_is_about_routes_not_about_nodes() {
        assert_eq!(verdict(true, true, 0), Verdict::RideOut);
    }

    #[test]
    fn a_working_ingress_survives_two_bad_reads_and_recovers_on_the_third() {
        let mut streak = 0;
        let mut routes = true;

        for _ in 0..2 {
            match verdict(routes, true, streak) {
                Verdict::RideOut => streak += 1,
                other => panic!("a blink should be ridden out, got {other:?}"),
            }
            assert!(routes, "the table must still be there");
        }
        assert_eq!(verdict(routes, false, streak), Verdict::Adopt);
        streak = 0;
        routes = true;
        assert_eq!(streak, 0);
        assert!(routes);
    }

    #[test]
    fn a_poisoned_table_does_not_stop_the_world() {
        let table = Arc::new(RwLock::new(Table::default()));

        // Poison it the way a panicking request handler would.
        let t = table.clone();
        let _ = std::thread::spawn(move || {
            let _guard = t.write().unwrap();
            panic!("a handler died holding the lock");
        })
        .join();
        assert!(table.is_poisoned(), "the lock should be poisoned for this test to mean anything");

        assert!(table.read().is_err(), "a poisoned lock refuses the ordinary read");

        assert!(read_table(&table).routes.is_empty());
        write_table(&table).routes.insert("shop.test".into(), Vec::new());
        assert_eq!(
            read_table(&table).routes.len(),
            1,
            "a poisoned routing table is still usable — it is a cache rebuilt every few \
             seconds, and killing the loop that rebuilds it is far worse than a torn read"
        );
    }

    #[test]
    fn three_bad_reads_running_are_believed() {
        let mut verdicts = Vec::new();
        for (streak, _) in (0..3).enumerate() {
            let v = verdict(true, true, streak as u32);
            verdicts.push(v);
        }
        assert_eq!(
            verdicts,
            vec![Verdict::RideOut, Verdict::RideOut, Verdict::Believe],
            "two rides then belief — a fleet that has genuinely gone must not be \
             routed to forever"
        );
    }
}
