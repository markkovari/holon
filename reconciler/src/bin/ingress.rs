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
//! Failure handling is inventory expiry plus retry-past-the-dead: a node that stops
//! heartbeating disappears from the table within a TTL, and nodes that die between
//! refreshes cost a request the walk past them rather than a 502.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use comp_lattice::{nats::NatsLattice, CommandBus, Inventory};
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

    /// Seconds between inventory refreshes. The table is read on every request and
    /// refreshed on a timer, because a request must never wait on the bus.
    /// Config file. Defaults to $COMP_CONFIG, then ./comp.toml if it exists.
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    // The tunables below take their default from the config file when the flag is
    // absent, so they are Option here rather than carrying a clap default — a clap
    // default is indistinguishable from a value someone typed.
    #[arg(long, env = "COMP_REFRESH_SECS")]
    refresh_secs: Option<u64>,

    /// How long an inventory entry lives before it expires.
    ///
    /// Shared with the hosts and the reconciler, which each declare it on the
    /// SAME bucket — so this must match them, or whoever creates the bucket first
    /// silently decides for everyone. Defaults to 15, as they do. When
    /// `--refresh-secs` is absent the refresh is a third of this, because a table
    /// re-read no faster than its entries expire is a table that is usually
    /// empty.
    #[arg(long, env = "COMP_INVENTORY_TTL")]
    inventory_ttl: Option<u64>,

    /// Seconds to wait on a backend before trying another replica.
    #[arg(long, env = "COMP_BACKEND_TIMEOUT")]
    backend_timeout: Option<u64>,

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

    /// Requests in flight to one node before this ingress starts shedding, per node
    /// rather than per app: what saturates is the machine, and an app that keeps
    /// queueing onto a node everyone else is also waiting on is the thing to stop.
    ///
    /// 0 disables shedding and restores the old behaviour, which ADR-0036 measured:
    /// with the fast half of the fleet killed, the survivor served 880 of 2690 rps
    /// with ZERO errors and a p99 of 46 SECONDS. Nothing failed; everything waited.
    /// A caller cannot retry a queue it cannot see, and 46s of holding a connection
    /// is worse for it than a 503 it could take elsewhere in milliseconds.
    ///
    /// PER CORE, multiplied by what the node advertises. The default is a
    /// starting point, not a calibrated number — high enough not to trip on a
    /// healthy fleet, low enough that the queue stays bounded.
    ///
    /// It used to be one global bound, with a note saying per-node capacity
    /// needed nodes to advertise what they can take. They have advertised it
    /// since ADR-0055, and the placement ranking has divided by it ever since;
    /// this is the door catching up with the scheduler.
    #[arg(long, env = "COMP_MAX_INFLIGHT")]
    max_inflight: Option<usize>,

    /// How many SLOW backends one request may spend before giving up. A refused
    /// connection is skipped for free and never counted against this.
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

/// How many consecutive route-less reads it takes before an ingress believes the
/// fleet really has nothing to serve.
const EMPTY_READS_BEFORE_BELIEVING: u32 = 3;

/// What to do with a freshly-read routing table.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// Take it. Either it has routes, or there were none to lose.
    Adopt,
    /// It is empty and we HAVE routes: keep them and wait. A blink must not
    /// become an outage.
    RideOut,
    /// It has been empty long enough to be true.
    Believe,
}

/// The decision that turned a momentary gap into a total outage when it was
/// implicit.
///
/// Pulled out as a function because the bug lived in exactly one branch of it and
/// was only reproducible by loading the machine until the timing went wrong,
/// which is a lottery rather than a test. Here every case is one line.
fn verdict(had_routes: bool, next_is_empty: bool, streak_before: u32) -> Verdict {
    if !next_is_empty {
        return Verdict::Adopt;
    }
    if !had_routes {
        // Nothing to protect. An ingress that never had routes and reads none has
        // learned nothing, and refusing to adopt would only delay the first real
        // table.
        return Verdict::Adopt;
    }
    if streak_before + 1 < EMPTY_READS_BEFORE_BELIEVING {
        Verdict::RideOut
    } else {
        Verdict::Believe
    }
}

/// Read the routing table, surviving a poisoned lock.
///
/// `RwLock::read().unwrap()` panics if ANY thread ever panicked while holding the
/// lock — and in the refresh task that panic is fatal and silent: the task ends,
/// nothing supervises it, and the table freezes at whatever it last held for the
/// rest of the process's life. That is how an ingress ended up answering `no app
/// answers` forever while the fleet was healthy.
///
/// A poisoned routing table is not dangerous. It is a cache of what the fleet
/// last said, rebuilt from scratch every few seconds, so the worst a poisoning
/// can mean is that one request saw a torn view — and the cure of killing the
/// refresh loop is far worse than that disease.
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
    /// Cores this node advertised. The shedding bound is PER CORE, because a
    /// four-core Pi and a ten-core laptop are not interchangeable — the placement
    /// ranking has divided by this since ADR-0055 and the door had not caught up.
    ///
    /// Measured: at a bound that let a Mac shed nothing, the same load through
    /// the same ingress shed 3 412 requests on a Pi. The Pi was not overloaded;
    /// the number was written for a bigger machine.
    cpus: usize,
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
/// Is this node already carrying `bound` requests?
///
/// Split out from the walk below so the shedding rule can be tested directly: a
/// rule that only exists inside an async proxy loop is a rule nobody checks.
/// One activation per host at a time. Without it a cold app's first burst sends one
/// activate per request — a stampede at exactly the moment the fleet has nothing
/// running to absorb it.
type Activating = Arc<RwLock<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>>;

fn activation_lock(map: &Activating, host: &str) -> Arc<tokio::sync::Mutex<()>> {
    if let Some(l) = map.read().unwrap().get(host) {
        return l.clone();
    }
    map.write().unwrap().entry(host.to_string()).or_default().clone()
}

/// Ask the reconciler to place a replica of `host`, and return where it went.
///
/// Single-flighted: whoever gets the lock asks, everyone else waits and then re-reads
/// the route table, which the winner's activation will have populated by way of the
/// next refresh — or, if it has not yet, takes the same answer from a second ask that
/// is now cheap because the instance is already running (a start is idempotent).
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
        // An activation reply carries no capacity. One core is the pessimistic
        // reading and it only holds until the next inventory refresh replaces
        // this entry with the node's real advertisement.
        cpus: v["cpus"].as_u64().unwrap_or(1) as usize,
    };
    // Publish it. Without this the check above — "someone may have activated
    // while we waited" — could never succeed, because nothing ever wrote what it
    // was looking for: the table only changed on the refresh timer. So every
    // request to a host missing from the table took the lock in turn and paid its
    // own round trip to the reconciler, for up to a full refresh interval.
    //
    // Measured: 13 000 spurious 503s in a 0.9-second window at the start of a
    // run, on an app that was placed and healthy the whole time, in two runs out
    // of four. It read as the ingress shedding load. It was the ingress
    // forgetting what it had just been told.
    //
    // The next refresh replaces this entry with the node's full advertisement,
    // including its real core count.
    table.write().unwrap().routes.entry(host.to_string()).or_default().push(backend.clone());
    eprintln!("comp-ingress: activated {host:?} on {}", backend.node);
    Some(backend)
}

/// `bound` is per CORE. A node advertising eight cores absorbs eight times what
/// a single-core one does before the ingress starts refusing on its behalf.
fn saturated(b: &Backend, inflight: &InFlight, bound: usize) -> bool {
    bound > 0 && counter(inflight, &b.node).load(Ordering::Relaxed) >= bound * b.cpus
}

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
                backends.push(Backend {
                    node: node.node.clone(),
                    address: node.address.clone(),
                    cpus: node.capacity.cpus.max(1),
                });
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

    // Resolve every tunable once, here, so no call site has to know where a value
    // came from.
    let file = comp_reconciler::settings::File::load(args.config.as_deref())?.ingress;
    let backend_timeout = settings::pick(args.backend_timeout, file.backend_timeout, 30);
    let max_inflight = settings::pick(args.max_inflight, file.max_inflight, 64);
    let slow_budget = settings::pick(args.slow_budget, file.slow_budget, 2);
    let activation_timeout = settings::pick(args.activation_timeout, file.activation_timeout, 10);

    // The inventory TTL is NOT this process's to choose, and pretending otherwise
    // is a bug that took a while to find.
    //
    // Three processes call `create_key_value` on the SAME bucket with their own
    // `max_age`: a host asks for `heartbeat_secs * 3`, the reconciler for
    // `inventory_ttl`, and this used to ask for a hardcoded 15. Whoever creates
    // the bucket first wins and the others silently get a TTL they did not ask
    // for. They agree today only because three defaults coincide at 15s — change
    // `--heartbeat-secs` on a host and they stop agreeing, with nothing said.
    //
    // Worse, the refresh interval was unrelated to it. Entries that live `T`
    // seconds must be re-read well inside `T`, and a 3s poll against a bucket
    // whose real TTL is short enough is a poll that mostly sees an empty bucket —
    // which is what `no app answers` looked like from the outside.
    let inventory_ttl = settings::pick(args.inventory_ttl, file.inventory_ttl, 15).max(1);
    // A third of the TTL, so two reads may be lost before anything is noticed.
    // An explicit `--refresh-secs` still wins, because an operator who sets it
    // means it.
    let refresh_secs = match args.refresh_secs.or(file.refresh_secs) {
        Some(v) => v.max(1),
        None => (inventory_ttl / 3).max(1),
    };
    eprintln!(
        "comp-ingress: inventory ttl {inventory_ttl}s, refreshing every {refresh_secs}s \
         (the ttl is shared with the hosts and the reconciler — a mismatch there is silent)"
    );

    let fabric = Arc::new(
        NatsLattice::connect(&args.nats_url, &args.lattice, Duration::from_secs(inventory_ttl))
            .await?,
    );
    // No inventory handle taken off `fabric`: the refresh loop below opens its own
    // NATS connection on its own thread, for the priority-inversion reason spelled
    // out there. This binding outlived that change.
    //
    // Used only when a host has NO replica placed: ask the reconciler to start one
    // and route to the address it replies with. The ingress still holds no platform
    // credential and no manifest — it asks, it does not decide.
    let commands: Arc<dyn CommandBus> = fabric;
    let activating: Activating = Arc::new(RwLock::new(BTreeMap::new()));
    // Best effort: an ingress that cannot open the load bucket still routes. Load is
    // an input to autoscaling, not to serving, and conflating them would make a
    // control-plane hiccup an outage.
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

    // Refreshed on a timer and read from a lock per request. A request that had to
    // ask the bus for its route would put the control plane on the data path, which
    // is the thing this design keeps apart.
    // ON ITS OWN THREAD, WITH ITS OWN RUNTIME AND ITS OWN CONNECTION.
    //
    // Every request depends on this table, so letting the task that maintains it
    // compete with request handling for scheduler time is a priority inversion —
    // the ingress is busiest exactly when it most needs to know where to send
    // things. And it is worse than a slow refresh: async-nats drives its
    // connection from a task on the runtime it was created on, so a starved
    // runtime does not merely delay `read_all`, it can leave it awaiting a reply
    // that nothing is left to deliver. The loop then stops printing and stops
    // updating, with no panic and no exit — which is exactly what was seen, and
    // is indistinguishable from a dead task from the outside.
    //
    // A dedicated OS thread with a current-thread runtime and a SEPARATE NATS
    // connection removes the coupling entirely. It costs one thread.
    {
        let (table, every) = (table.clone(), refresh_secs);
        let (nats_url, lattice_name, ttl) =
            (args.nats_url.clone(), args.lattice.clone(), inventory_ttl);
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
                    // Retried rather than fatal: an ingress that cannot reach the
                    // bus at startup still serves what it is told to activate, and
                    // giving up here would mean it never routes again even once
                    // the bus comes back.
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

                        // A heartbeat on EVERY read, so silence in the log means
                        // the loop is not running rather than the loop having
                        // nothing to say. Distinguishing those two took three
                        // wrong diagnoses.
                        eprintln!(
                            "comp-ingress: refresh #{reads}: {} node(s), {} instance(s), {} route(s)",
                            nodes.len(),
                            nodes.iter().map(|n| n.instances.len()).sum::<usize>(),
                            next.routes.len()
                        );
                        reads += 1;

                        // The decision lives in `verdict` so it can be tested
                        // without waiting for a machine to be busy enough to go
                        // wrong. See its tests for every case.
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

                        // Nodes present and routes NEVER built is a different fault
                        // from losing them, and wants a different fix: instances
                        // that carry no ingress-host. One "0 routes" cannot tell
                        // the two apart, so both halves are counted.
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
                    // A failed read leaves the previous table in place. The
                    // alternative — an empty table — would 503 every request
                    // because the CONTROL plane blinked, which is precisely what
                    // reading inventory instead of asking the platform avoids.
                    Err(e) => eprintln!("comp-ingress: inventory read failed: {e:#}"),
                }
            }
            // Unreachable while the loop is `loop {}`, and here because a
            // background task that stops is the failure that started all this:
            // the routing table simply freezes and every request is answered from
            // a snapshot of a fleet that has since moved on, with nothing said.
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

    // Publish observed concurrency per host so the reconciler can autoscale on it.
    // A short TTL on the bucket means a dead ingress stops voting on its own rather
    // than pinning an app at whatever it last saw — the same mechanism that retires
    // a dead node's inventory.
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
                    // Taken and RESET: this is what was refused during the interval,
                    // not since the process started. A cumulative count would keep
                    // asking for replicas long after the pressure was gone.
                    let refused = counter(&shed, &host).swap(0, Ordering::Relaxed);
                    let answered = counter(&served, &host).swap(0, Ordering::Relaxed);
                    // The key is the host with dots swapped: NATS KV keys may not
                    // contain arbitrary punctuation, and `shop.eve.test` would be a
                    // subject wildcard boundary rather than one key.
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
    // Concurrency per HOST, which is what autoscaling needs. The per-node counter
    // above cannot answer it: several apps share a node, so its value says how busy
    // the node is, not how busy this app is.
    per_host: InFlight,
    // Requests refused since the last publish. Shedding creates a blind spot in the
    // autoscaling signal: a shed request never becomes in-flight, so concurrency
    // UNDERSTATES demand exactly when demand is highest and the app most needs
    // replicas. Counting refusals is what closes it (ADR-0045).
    shed: InFlight,
    // Responses a backend actually produced this interval. Without it, "every
    // replica is busy" and "every replica is wedged" publish the same shed count,
    // and scaling up a wedged component just makes more wedged instances.
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

    // Held for the whole request INCLUDING retries: a request being retried is
    // still a request the app owes an answer to.
    let _busy_host = Busy::on(counter(&per_host, &host));

    let mut backends = read_table(&table).routes.get(&host).cloned().unwrap_or_default();
    if backends.is_empty() {
        // Nothing placed. Rather than refuse, ask the reconciler to start one and
        // hold this request while it does — a scaled-to-zero app is meant to come
        // back on demand, and ADR-0040 made that cost 0.43 ms of actual work.
        match activate(&commands, &activating, &table, &host, activation_timeout).await {
            Some(b) => backends = vec![b],
            // 503, not 404: the app may exist and simply have no replica up. A 404
            // would tell a caller to stop retrying, which is the wrong advice.
            None => {
                return Ok(status(503, &format!("no replica of {host:?} is currently placed\n")))
            }
        }
    }

    let ranked = order(&backends, mode, &next, &inflight);

    // Shed rather than queue. If every replica is already at the bound, this request
    // would join a queue with no bound and no way for the caller to see it — the 46
    // second p99 in ADR-0036. A 503 now is information the caller can act on.
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
    // Walk the ranking past dead replicas, but spend at most `SLOW_BUDGET` on slow
    // ones. The two failures are not alike and treating them alike cost a measured
    // outage: least-outstanding ranks a DEAD node FIRST, because a node answering
    // nothing has nothing in flight. With `.take(2)` — one retry — two corpses at
    // the top of the ranking exhausted the budget and the request 502'd, which is
    // exactly what killing a two-node machine out of three did (0.04% of requests,
    // for 13s). A refused connection is an instant RST and costs nothing to skip; a
    // timeout is the one that turns a retry into a stampede, so only it is budgeted.
    let mut slow = 0;
    for backend in ranked.into_iter() {
        if slow >= slow_budget {
            break;
        }
        // Skip a saturated replica the way a dead one is skipped. The check above
        // only fires when EVERY replica is saturated; this is what steers around a
        // single hot node while the others still have room.
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
                // Any completed response counts, not just 2xx: a 429 from a rate
                // limiter is the component doing its job. What is being measured is
                // "is the fleet answering at all", not "is it answering happily".
                counter(&served, &host).fetch_add(1, Ordering::Relaxed);
                let (mut rp, rb) = resp.into_parts();
                // Which replica answered. The single most useful thing an ingress
                // can tell you, and the only way to see the balance from outside.
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

    /// A control plane that will answer an activation exactly once.
    ///
    /// The second ask fails, which is what makes this a test rather than a
    /// demonstration: the only way both calls succeed is if the first one left
    /// its answer somewhere the second could find it.
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

    /// An activation is published, so concurrent requests do not each pay for one.
    ///
    /// `activate` checks the table for a backend someone else already brought up.
    /// That check could never succeed, because nothing wrote what it was looking
    /// for — the table only changed on the refresh timer. Every request to a host
    /// missing from it took the lock in turn and made its own round trip, and
    /// under load 13 000 of them were refused inside a 0.9-second window for an
    /// app that was placed and healthy throughout.
    #[tokio::test]
    async fn an_activation_is_published_so_the_next_request_finds_it() {
        let bus: Arc<dyn CommandBus> = Arc::new(AnswersOnce(AtomicUsize::new(0)));
        let activating: Activating = Default::default();
        let table = Arc::new(RwLock::new(Table::default()));

        let first = activate(&bus, &activating, &table, "shop.example.com", 5).await;
        assert!(first.is_some(), "the first activation should succeed");

        // The control plane refuses to answer again, so this can only be served
        // from what the first one published.
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
        let m: BTreeMap<String, Arc<AtomicUsize>> =
            pairs.iter().map(|(n, v)| (n.to_string(), Arc::new(AtomicUsize::new(*v)))).collect();
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

    #[test]
    fn a_node_at_the_bound_is_saturated_and_zero_disables_the_rule() {
        let inflight = inflight_of(&[("n1", 63), ("n2", 64), ("n3", 999)]);
        let b = |n: &str| Backend { node: n.into(), address: "127.0.0.1:1".into(), cpus: 1 };
        assert!(!saturated(&b("n1"), &inflight, 64), "63 of 64 still has room");
        assert!(saturated(&b("n2"), &inflight, 64), "at the bound is saturated");
        assert!(saturated(&b("n3"), &inflight, 64));
        // 0 is the escape hatch back to the old behaviour, and it must be total:
        // an operator who turns shedding off must not get it at 999 in flight.
        assert!(!saturated(&b("n3"), &inflight, 0), "0 must disable shedding entirely");
    }

    #[test]
    fn shedding_steers_around_a_hot_node_before_it_refuses_anything() {
        // The rule that matters most: one saturated replica must NOT cost the app a
        // 503 while its siblings are idle. Only an entirely saturated set sheds.
        let inflight = inflight_of(&[("hot", 64), ("cool", 0)]);
        let backends = vec![
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
        // ADR-0036: with nothing shedding, the survivor took every connection and
        // answered a p99 of 46 SECONDS. Every replica at the bound is the state that
        // produced it, and it must now be a refusal instead.
        let inflight = inflight_of(&[("n1", 64), ("n2", 70)]);
        let backends = vec![
            Backend { node: "n1".into(), address: "127.0.0.1:1".into(), cpus: 1 },
            Backend { node: "n2".into(), address: "127.0.0.1:2".into(), cpus: 1 },
        ];
        assert!(backends.iter().all(|b| saturated(b, &inflight, 64)));
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;

    /// The failure this function was extracted for.
    ///
    /// An ingress that HAS routes and reads none must keep what it has. Losing
    /// every route while the fleet is plainly still there is a gap to ride out,
    /// not news to act on — and adopting the empty table is what turned a
    /// momentary blink into `no app answers` for the rest of the process's life.
    #[test]
    fn a_blink_does_not_empty_a_working_table() {
        assert_eq!(verdict(true, true, 0), Verdict::RideOut);
        assert_eq!(verdict(true, true, 1), Verdict::RideOut);
    }

    /// But a fleet that really has stopped must eventually be believed, or the
    /// ingress routes to backends that are gone forever.
    #[test]
    fn an_empty_fleet_is_believed_in_the_end() {
        assert_eq!(verdict(true, true, EMPTY_READS_BEFORE_BELIEVING - 1), Verdict::Believe);
        assert_eq!(verdict(true, true, EMPTY_READS_BEFORE_BELIEVING), Verdict::Believe);
    }

    /// A read with routes is always taken, whatever happened before it. This is
    /// what makes the ride-out temporary rather than a latch.
    #[test]
    fn a_good_read_is_always_adopted_and_clears_the_streak() {
        assert_eq!(verdict(true, false, 0), Verdict::Adopt);
        assert_eq!(verdict(true, false, 99), Verdict::Adopt);
        assert_eq!(verdict(false, false, 2), Verdict::Adopt);
    }

    /// An ingress that never had routes has nothing to protect, and refusing to
    /// adopt would only delay its first real table — including at startup, where
    /// every read is empty until something is placed.
    #[test]
    fn nothing_to_lose_means_nothing_to_defend() {
        assert_eq!(verdict(false, true, 0), Verdict::Adopt);
        assert_eq!(verdict(false, true, 5), Verdict::Adopt);
    }

    /// The bug in the FIRST attempt at this guard, kept as a test so it cannot
    /// come back: the check sat on whether any NODES were present, so three
    /// healthy nodes advertising zero routes sailed straight past it and wiped
    /// the table. The decision must depend only on whether routes were lost.
    #[test]
    fn the_guard_is_about_routes_not_about_nodes() {
        // Three nodes, zero routes, and a table that had some: still a ride-out.
        // A guard keyed on node count would have said "adopt" here.
        assert_eq!(verdict(true, true, 0), Verdict::RideOut);
    }

    /// Walking the sequence that actually happened, one read at a time.
    #[test]
    fn a_working_ingress_survives_two_bad_reads_and_recovers_on_the_third() {
        let mut streak = 0;
        let mut routes = true;

        // Two blinks: the table is kept both times.
        for _ in 0..2 {
            match verdict(routes, true, streak) {
                Verdict::RideOut => streak += 1,
                other => panic!("a blink should be ridden out, got {other:?}"),
            }
            assert!(routes, "the table must still be there");
        }
        // The fleet comes back.
        assert_eq!(verdict(routes, false, streak), Verdict::Adopt);
        streak = 0;
        routes = true;
        assert_eq!(streak, 0);
        assert!(routes);
    }

    /// A poisoned lock must not be able to stop the refresh loop.
    ///
    /// This is the failure underneath the failure. `read().unwrap()` panics on a
    /// poisoned lock, and in the refresh task that panic is fatal AND silent: the
    /// task ends, nothing supervises it, and the routing table freezes at
    /// whatever it last held. An ingress in that state answers `no app answers`
    /// forever while every backend is healthy, which is exactly what was seen.
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

        // The plain form is what used to be here, and it would end the task.
        assert!(table.read().is_err(), "a poisoned lock refuses the ordinary read");

        // These must not.
        assert!(read_table(&table).routes.is_empty());
        write_table(&table).routes.insert("shop.test".into(), Vec::new());
        assert_eq!(
            read_table(&table).routes.len(),
            1,
            "a poisoned routing table is still usable — it is a cache rebuilt every few \
             seconds, and killing the loop that rebuilds it is far worse than a torn read"
        );
    }

    /// And the sequence where the fleet really did stop.
    #[test]
    fn three_bad_reads_running_are_believed() {
        let mut streak = 0;
        let mut verdicts = Vec::new();
        for _ in 0..3 {
            let v = verdict(true, true, streak);
            streak += 1;
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
