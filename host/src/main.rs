//! `comp-host` — a NATIVE Rust server that runs ANY composed wasm component over
//! wasmtime. No Node, no Kubernetes, no wasmCloud, no NATS required: this binary IS
//! the host.
//!
//! It started life running one app (hence its old name, `vet-host`) and is now the
//! self-hosting lane for anything in this repo: point it at a composed artifact from
//! `just compose-<app>`, and it serves that component's
//! `wasi:http/incoming-handler` export over a hyper TCP listener while satisfying
//! the component's imports host-side:
//!   - standard WASI (cli/clocks/random/io/filesystem) via wasmtime-wasi
//!   - wasi:http via wasmtime-wasi-http
//!   - wasi:keyvalue@0.2.0-draft  -> memory, sqlite, redis or NATS (--kv). Two axes,
//!     and they are not the same: `memory` loses everything on restart, and
//!     `memory`/`sqlite` are NODE-LOCAL, so two replicas of one app on two nodes get
//!     two stores under one bucket name. The reconciler refuses to spread a stateful
//!     app onto them (docs/adr/0027).
//!   - wasi:config@0.2.0-draft    -> per-instance, from the start command or
//!     `--config k=v` / `--config-file`. NOT the process environment: in a host
//!     shared by every tenant on the node that would be a cross-tenant read.
//!
//! Two lanes, one binary: without `--lattice-nats` it serves one `--component`;
//! with it, instances arrive as start commands and it holds every tenant on the
//! node at once (docs/adr/0021, 0023).

mod agent;
mod kv;
mod rpc;
mod tenant;
use kv::KvBackend;
use tenant::{BucketId, InstanceId, Limits, SharedScope, StartCommand};

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Context as _, Result};
use clap::Parser;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use wasmtime::component::{Component, HasSelf, Linker, Resource, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::p2::bindings::http::types::ErrorCode;
use wasmtime_wasi_http::p2::bindings::ProxyPre;
use wasmtime_wasi_http::p2::body::HyperOutgoingBody;
use wasmtime_wasi_http::p2::types::{HostFutureIncomingResponse, OutgoingRequestConfig};
use wasmtime_wasi_http::p2::{
    default_send_request_handler, HttpResult, WasiHttpCtxView, WasiHttpHooks, WasiHttpView,
};
use wasmtime_wasi_http::WasiHttpCtx;

// Generate host traits for the non-standard imports from host/wit/host.wit.
mod bindings {
    wasmtime::component::bindgen!({
        path: "wit",
        world: "host-imports",
        // wasmtime >=34 folded `async` + `trappable_imports` into one knob;
        // sync is the default, `trappable` keeps imports returning Result.
        imports: { default: trappable },
        with: {
            // wasmtime >=34 keys a resource as `interface.resource`, not `interface/resource`.
            "wasi:keyvalue/store.bucket": super::HostBucket,
        },
    });
}

use bindings::cache::store::sink as cache_sink;
use bindings::cache::store::source as cache_source;
use bindings::wasi::config::store as config;
use bindings::wasi::keyvalue::atomics;
use bindings::wasi::keyvalue::batch;
use bindings::wasi::keyvalue::store;

// ---- the key-value store -------------------------------------------------
// The guest's wasi:keyvalue is backed by a swappable `KvBackend` (memory /
// redis / nats — chosen by `--kv`). The component bytes never change; only this
// host-side impl does. (See kv.rs.)

pub type Kv = Arc<dyn KvBackend>;
/// the cache component's backing store (flat key -> bytes), shares the same Kv
/// under a reserved bucket.
pub type CacheBacking = Arc<Mutex<HashMap<String, Vec<u8>>>>;

// ---- the instance table ---------------------------------------------------

/// One running component: its scope, and the pre-instantiated world it serves.
///
/// `InstancePre` is built once, when the instance starts — not per request. That
/// is the single biggest difference from the old one-app host, which did it at
/// boot for the only component it would ever run.
pub struct Instance {
    pub scope: SharedScope,
    pub pre: ProxyPre<Host>,
    /// Clients for this instance's REMOTE imports, keyed by interface. Built once
    /// at start, because resolving a target per request would put a lookup on the
    /// hot path for something that only changes when placement does.
    pub remotes: std::collections::BTreeMap<String, wrpc_transport_nats::Client>,
    /// How many replicas this node was asked for.
    ///
    /// Not N copies of anything: one `InstancePre` already serves concurrent
    /// requests, so on this runtime a replica is a share of the pool rather than a
    /// process. It is tracked because the reconciler counts replicas per node, and
    /// a host that always reported 1 would be asked to start a second one forever.
    pub count: u32,
}

/// Everything running on this node, by `<tenant>/<app>/<component>`.
///
/// An `RwLock<HashMap>` because reads are the hot path — every request takes one —
/// and a read is ~20ns against the millisecond it guards. Writes happen at
/// start-command rates, which is to say almost never.
// ponytail: RwLock<HashMap>; arc-swap if start/stop ever contends with traffic,
// which at these write rates it will not.
pub type Instances = Arc<RwLock<HashMap<InstanceId, Arc<Instance>>>>;

/// `Host` header -> instance. The door into the node.
pub type Routes = Arc<RwLock<HashMap<String, InstanceId>>>;

/// A host resource handed to the guest when it calls `store.open(name)`.
///
/// It carries a `BucketId`, not the name the guest asked for. That is the whole
/// ADR-0012 fix, and putting it on the RESOURCE rather than in each method is what
/// makes `atomics` and `batch` inherit it — they re-read the handle, so there is no
/// sibling caller left holding a guest string.
pub struct HostBucket {
    id: BucketId,
}

// ---- the per-request store state -----------------------------------------

struct Host {
    table: ResourceTable,
    wasi: WasiCtx,
    http: WasiHttpCtx,
    /// Outbound HTTP policy. This used to be `[(); 0]` — upstream's zero-sized
    /// default, i.e. unrestricted egress. In a process shared by every tenant on
    /// the node, unrestricted egress reaches the NATS bus, this host's own
    /// listener, and every other node on the tailnet.
    hooks: Egress,
    kv: Kv,
    cache_backing: CacheBacking,
    /// Who this instance is and what it may touch. Built by the host at start time
    /// from a control-plane record; never from guest input, never from `std::env`.
    scope: SharedScope,
    /// Per-store resource ceiling. Set from the scope in `handle_request`.
    limits: wasmtime::StoreLimits,
    /// How this store reaches components on other nodes. `Solo` off a lattice, so
    /// the single-app lane needs no broker and says so if something tries.
    rpc: rpc::RpcCtx,
}

impl wrpc_runtime_wasmtime::WrpcView for Host {
    type Invoke = rpc::Transport;

    fn wrpc(&mut self) -> wrpc_runtime_wasmtime::WrpcCtxView<'_, Self::Invoke> {
        wrpc_runtime_wasmtime::WrpcCtxView { ctx: &mut self.rpc, table: &mut self.table }
    }
}

/// Map a backend error to the wasi:keyvalue `error` variant.
fn kv_err(e: anyhow::Error) -> store::Error {
    store::Error::Other(format!("{e:#}"))
}

// wasmtime 47 projects the ctx AND the resource table together, so a host can
// hand out both with one borrow instead of two methods that alias `self`.
impl WasiView for Host {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
    }
}
impl WasiHttpView for Host {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView { ctx: &mut self.http, table: &mut self.table, hooks: &mut self.hooks }
    }
}

// ---- wasi:keyvalue/store host impl ---------------------------------------

impl store::Host for Host {
    /// The one function ADR-0012 was about.
    ///
    /// It used to push `HostBucket { name: identifier }` — the guest's own string
    /// became the bucket, so two tenants running the same component read the same
    /// records. Now the string is a KEY INTO HOST STATE and nothing else: a hit
    /// yields the store the platform assigned, a miss yields `no-such-store`.
    ///
    /// A miss must never fall back to the default. A fallback would mean a guest
    /// naming its neighbour's bucket gets *a* bucket rather than an error, which is
    /// the same class of bug wearing an apology.
    fn open(&mut self, identifier: String) -> wasmtime::Result<Result<Resource<HostBucket>, store::Error>> {
        let Some(id) = self.scope.bucket(&identifier) else {
            return Ok(Err(store::Error::NoSuchStore));
        };
        let res = self.table.push(HostBucket { id: id.clone() })?;
        Ok(Ok(res))
    }
}

impl store::HostBucket for Host {
    fn get(
        &mut self,
        self_: Resource<HostBucket>,
        key: String,
    ) -> wasmtime::Result<Result<Option<Vec<u8>>, store::Error>> {
        let id = self.table.get(&self_)?.id.clone();
        Ok(self.kv.get(&id, &key).map_err(kv_err))
    }

    fn set(
        &mut self,
        self_: Resource<HostBucket>,
        key: String,
        value: Vec<u8>,
    ) -> wasmtime::Result<Result<(), store::Error>> {
        let id = self.table.get(&self_)?.id.clone();
        Ok(self.kv.set(&id, &key, &value).map_err(kv_err))
    }

    fn delete(
        &mut self,
        self_: Resource<HostBucket>,
        key: String,
    ) -> wasmtime::Result<Result<(), store::Error>> {
        let id = self.table.get(&self_)?.id.clone();
        Ok(self.kv.delete(&id, &key).map_err(kv_err))
    }

    fn exists(
        &mut self,
        self_: Resource<HostBucket>,
        key: String,
    ) -> wasmtime::Result<Result<bool, store::Error>> {
        let id = self.table.get(&self_)?.id.clone();
        Ok(self.kv.exists(&id, &key).map_err(kv_err))
    }

    fn list_keys(
        &mut self,
        self_: Resource<HostBucket>,
        _cursor: Option<u64>,
    ) -> wasmtime::Result<Result<store::KeyResponse, store::Error>> {
        let id = self.table.get(&self_)?.id.clone();
        Ok(self
            .kv
            .list_keys(&id)
            .map(|keys| store::KeyResponse { keys, cursor: None })
            .map_err(kv_err))
    }

    fn drop(&mut self, rep: Resource<HostBucket>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

// ---- wasi:keyvalue/atomics host impl -------------------------------------

impl atomics::Host for Host {
    fn increment(
        &mut self,
        bucket: Resource<HostBucket>,
        key: String,
        delta: u64,
    ) -> wasmtime::Result<Result<u64, store::Error>> {
        let id = self.table.get(&bucket)?.id.clone();
        Ok(self.kv.increment(&id, &key, delta).map_err(kv_err))
    }
}

// ---- wasi:keyvalue/batch host impl ----------------------------------------
// One guest call instead of N for multi-key reads/writes. The backend loop is
// host-side, so even without a backend-native multi-get this removes the
// per-key guest<->host round-trips.

impl batch::Host for Host {
    fn get_many(
        &mut self,
        bucket: Resource<HostBucket>,
        keys: Vec<String>,
    ) -> wasmtime::Result<Result<Vec<Option<(String, Vec<u8>)>>, store::Error>> {
        let id = self.table.get(&bucket)?.id.clone();
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            match self.kv.get(&id, &key) {
                Ok(Some(v)) => out.push(Some((key, v))),
                Ok(None) => out.push(None),
                Err(e) => return Ok(Err(kv_err(e))),
            }
        }
        Ok(Ok(out))
    }

    fn set_many(
        &mut self,
        bucket: Resource<HostBucket>,
        key_values: Vec<(String, Vec<u8>)>,
    ) -> wasmtime::Result<Result<(), store::Error>> {
        let id = self.table.get(&bucket)?.id.clone();
        for (key, value) in key_values {
            if let Err(e) = self.kv.set(&id, &key, &value) {
                return Ok(Err(kv_err(e)));
            }
        }
        Ok(Ok(()))
    }

    fn delete_many(
        &mut self,
        bucket: Resource<HostBucket>,
        keys: Vec<String>,
    ) -> wasmtime::Result<Result<(), store::Error>> {
        let id = self.table.get(&bucket)?.id.clone();
        for key in keys {
            if let Err(e) = self.kv.delete(&id, &key) {
                return Ok(Err(kv_err(e)));
            }
        }
        Ok(Ok(()))
    }
}

// ---- cache:store source + sink host impl (the cache backing store) -------

impl cache_source::Host for Host {
    fn load(&mut self, key: String) -> wasmtime::Result<Result<Option<Vec<u8>>, String>> {
        Ok(Ok(self.cache_backing.lock().unwrap().get(&key).cloned()))
    }
}
impl cache_sink::Host for Host {
    fn store(&mut self, key: String, value: Vec<u8>) -> wasmtime::Result<Result<(), String>> {
        self.cache_backing.lock().unwrap().insert(key, value);
        Ok(Ok(()))
    }
    fn remove(&mut self, key: String) -> wasmtime::Result<Result<(), String>> {
        self.cache_backing.lock().unwrap().remove(&key);
        Ok(Ok(()))
    }
}

// ---- wasi:config/store host impl ---------------------------------------

impl config::Host for Host {
    fn get(&mut self, key: String) -> wasmtime::Result<Result<Option<String>, config::Error>> {
        Ok(Ok(self.scope.cfg.get(&key).cloned()))
    }
    fn get_all(&mut self) -> wasmtime::Result<Result<Vec<(String, String)>, config::Error>> {
        Ok(Ok(self.scope.cfg.iter().map(|(k, v)| (k.clone(), v.clone())).collect()))
    }
}

// ---- egress: the wasi:http outbound allow-list ---------------------------

/// Outbound HTTP policy for one instance.
///
/// Two checks, because one is not enough. The **name** must be on the app's
/// allow-list, which is what an operator can reason about. Then every address it
/// **resolves to** must be outside the ranges no tenant may reach — because a name
/// check alone is satisfied by pointing an allow-listed name at `169.254.169.254`.
struct Egress {
    scope: SharedScope,
}

impl WasiHttpHooks for Egress {
    fn send_request(
        &mut self,
        request: hyper::Request<HyperOutgoingBody>,
        config: OutgoingRequestConfig,
    ) -> HttpResult<HostFutureIncomingResponse> {
        let authority = request
            .uri()
            .authority()
            .map(|a| a.as_str().to_string())
            .or_else(|| {
                request
                    .headers()
                    .get(hyper::header::HOST)
                    .and_then(|h| h.to_str().ok())
                    .map(str::to_string)
            })
            .unwrap_or_default();

        // Fail closed and fail early: no allow-list entry means no connection is
        // ever opened, so this costs nothing when it refuses.
        if !self.scope.egress.permits_authority(&authority) {
            eprintln!(
                "comp-host: {} denied egress to {authority:?} (not on its allow-list)",
                self.scope.id()
            );
            return Err(ErrorCode::HttpRequestDenied.into());
        }

        let scope = self.scope.clone();
        let port = if config.use_tls { 443 } else { 80 };
        let handle = wasmtime_wasi::runtime::spawn(async move {
            let target =
                if authority.contains(':') { authority.clone() } else { format!("{authority}:{port}") };
            match tokio::net::lookup_host(&target).await {
                Ok(addrs) => {
                    for a in addrs {
                        if !scope.egress.permits_addr(a.ip()) {
                            eprintln!(
                                "comp-host: {} denied egress to {target} — it resolves to {}",
                                scope.id(),
                                a.ip()
                            );
                            return Ok(Err(ErrorCode::DestinationIpProhibited));
                        }
                    }
                }
                Err(_) => return Ok(Err(ErrorCode::DestinationUnavailable)),
            }
            // ponytail: the connect re-resolves, so a DNS-rebinding attacker who
            // controls an allow-listed name can still land on a denied address
            // between this check and the dial. The real fix is a connector pinned
            // to the address we checked; do it if egress ever guards something an
            // attacker would spend a rebind on.
            Ok(default_send_request_handler(request, config).await)
        });
        Ok(HostFutureIncomingResponse::pending(handle))
    }
}

// ---- config ---------------------------------------------------------------
//
// `build_config()` used to live here, reading `wasi:config` out of the process
// environment (a `VET_*` block plus a generic `CFG_*` passthrough). It is gone
// rather than scoped: in a process shared by every tenant on the node, process
// environment as config is a cross-tenant read by construction — one `getenv` and
// every app sees every other app's knobs, including the ones people put secrets in.
//
// Config now arrives per component in the start command and lives on the `Scope`.
// For a single-app run, `--config k=v` supplies it (see `Args`).

// ---- CLI -----------------------------------------------------------------

#[derive(Parser)]
#[command(name = "comp-host", about = "Run the composed vet-domain wasm over wasmtime")]
struct Args {
    /// Path to the composed component wasm.
    #[arg(long, default_value = "../components/target/vet_domain.composed.wasm")]
    component: String,
    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1:3007")]
    addr: String,
    /// Optional directory of static files (a built SPA) to serve for GET
    /// requests that aren't API routes. Omit for API-only.
    #[arg(long)]
    static_dir: Option<String>,
    /// Key-value backend: memory | sqlite | redis | nats. The wasm component is
    /// identical for all four — only the host store changes.
    ///
    /// Defaults to `nats` on a lattice node and `memory` for a single-app run. That
    /// difference is deliberate: NATS is already mandatory on a lattice, and it is
    /// the only backend where two replicas of one app share a store, so defaulting a
    /// node to anything else hands you either silent divergence (ADR-0027) or, with
    /// `memory`, silent loss on the next restart.
    #[arg(long)]
    kv: Option<String>,
    /// Redis URL for --kv redis.
    #[arg(long, default_value = "redis://127.0.0.1:6379")]
    redis_url: String,
    /// File for `--kv sqlite`. Defaults to `$STATE_DIRECTORY/kv.db`, which systemd
    /// sets for a unit with `StateDirectory=` — private to the app's uid under
    /// `DynamicUser=yes`. Falls back to ./comp-kv.db when run by hand.
    #[arg(long)]
    sqlite_path: Option<String>,
    /// NATS URL for `--kv nats`. Defaults to the lattice's own NATS when
    /// `--lattice-nats` is given, because running a node's store on a different
    /// cluster from its control bus is a thing to do on purpose, not by default.
    #[arg(long)]
    nats_url: Option<String>,
    /// Use wasmtime's POOLING allocator (pre-reserved instance/memory slots,
    /// reused across requests) instead of the default on-demand allocator. This
    /// is what wasmCloud does — it makes per-request component instantiation of
    /// the 19-component graph far cheaper. Off by default (the naive baseline).
    #[arg(long)]
    pool: bool,

    // ---- who this instance is -------------------------------------------
    /// Tenant this component belongs to. With `--app` it decides the store the
    /// guest's `open("default")` resolves to, so two hosts started with different
    /// tenants cannot see each other's records even on one backend.
    #[arg(long, default_value = "local")]
    tenant: String,
    /// Application name. See `--tenant`.
    #[arg(long, default_value = "app")]
    app: String,
    /// Config for `wasi:config`, repeatable: `--config grace-period-secs=5`.
    /// Replaces the old `CFG_*`/`VET_*` environment scrape, which in a shared
    /// process was a cross-tenant read.
    #[arg(long = "config", value_parser = parse_kv)]
    configs: Vec<(String, String)>,
    /// A file of `key = value` lines to seed config from, applied BEFORE `--config`
    /// so individual flags win. This is how the example suites get the block of
    /// defaults `build_config()` used to hardcode, without a host that knows
    /// anything about any particular app.
    #[arg(long)]
    config_file: Option<String>,
    /// Authorities this component may reach over `wasi:http`, repeatable.
    /// DEFAULT-DENY: with none given the component has no outbound HTTP at all.
    /// `--egress '*'` opts out of the name check (never out of the address one).
    #[arg(long = "egress")]
    egress: Vec<String>,
    /// Let a component reach private networks — loopback, RFC1918, link-local,
    /// and Tailscale's CGNAT range. Off by default because on a lattice node those
    /// are the NATS bus, the cloud metadata endpoint, and every other node.
    /// Development only.
    #[arg(long)]
    allow_private_egress: bool,
    /// Per-instance linear memory ceiling, in MiB.
    #[arg(long, default_value = "64")]
    mem_cap_mb: usize,
    /// How long a guest may run before it is made to yield, in milliseconds.
    /// Fairness, not a request timeout: one tenant's hot loop must not starve the
    /// node it shares.
    #[arg(long, default_value = "50")]
    slice_ms: u64,

    // ---- lattice mode ----------------------------------------------------
    /// Join a lattice at this NATS URL and take instances from start commands
    /// instead of `--component`. Omit for the single-app lane, which is unchanged
    /// and needs no NATS at all.
    #[arg(long)]
    lattice_nats: Option<String>,
    /// This node's name on the lattice. Defaults to the machine's hostname.
    #[arg(long)]
    node: Option<String>,
    /// Lattice name; must match the reconciler's.
    #[arg(long, default_value = "default")]
    lattice: String,
    /// Labels this node advertises, repeatable: `--label region=eu-central`.
    /// A deployment's placement constraints are matched against these.
    #[arg(long = "label", value_parser = parse_kv)]
    labels: Vec<(String, String)>,
    /// Where artifacts and the instance ledger live. Defaults to
    /// `$STATE_DIRECTORY`, which systemd sets for a unit with `StateDirectory=`.
    #[arg(long)]
    state_dir: Option<String>,
    /// Seconds between inventory publications. The bucket expires an entry after
    /// three of these, which is how a departed node disappears.
    #[arg(long, default_value = "5")]
    heartbeat_secs: u64,
    /// Where an ingress can reach this node, `host:port`. Defaults to `--addr`,
    /// which is right for a loopback or explicit bind and wrong for `0.0.0.0` — a
    /// node bound to every interface knows its port and not its address, so a real
    /// deployment passes this.
    #[arg(long)]
    advertise_addr: Option<String>,
}

/// `k=v`, for `--config`.
fn parse_kv(s: &str) -> std::result::Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.trim().to_string(), v.to_string()))
        .ok_or_else(|| format!("expected key=value, got {s:?}"))
}

/// `key = value` lines, `#` comments, blanks ignored. Deliberately not TOML: the
/// values are all strings (that is what `wasi:config` is), so a parser would only
/// add a dependency and a way to write something that cannot be represented.
fn parse_config_file(text: &str) -> std::result::Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| format!("line {}: expected key = value, got {line:?}", i + 1))?;
        out.push((k.trim().to_string(), v.trim().to_string()));
    }
    Ok(out)
}

/// The one place host capabilities are granted.
///
/// Everything a tenant's component can reach is on this list and nothing else is —
/// an import with no entry here and no link-table entry means the instance refuses
/// to start. `agent::HOST_IFACES` is the advertised form of the same list; the two
/// must agree, and a test asserts the shape of it.
pub fn build_linker(engine: &Engine) -> Result<Linker<Host>> {
    let mut linker: Linker<Host> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
    store::add_to_linker::<_, HasSelf<Host>>(&mut linker, |h| h)?;
    atomics::add_to_linker::<_, HasSelf<Host>>(&mut linker, |h| h)?;
    batch::add_to_linker::<_, HasSelf<Host>>(&mut linker, |h| h)?;
    config::add_to_linker::<_, HasSelf<Host>>(&mut linker, |h| h)?;
    cache_source::add_to_linker::<_, HasSelf<Host>>(&mut linker, |h| h)?;
    cache_sink::add_to_linker::<_, HasSelf<Host>>(&mut linker, |h| h)?;
    Ok(linker)
}

// ---- main: instantiate + serve -------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let mut wt_config = Config::new();
    // A guest that will not yield must be made to. One tenant's hot loop is now
    // one tenant's hot loop on a box holding everyone else's apps, so this is
    // fairness rather than a nicety. Epoch, not fuel: fuel meters instructions,
    // which is the right tool for a bill and the wrong one for a scheduler.
    wt_config.epoch_interruption(true);
    // (wasmtime >=47: async support is unconditional; `async_support` is a no-op.)
    if args.pool {
        // wasmtime's pooling allocator: pre-reserve a fixed set of instance +
        // memory + table slots and recycle them, so instantiating the
        // 19-component graph per request costs a slot grab, not fresh mmaps.
        // (The strategy wasmCloud uses.) Generous caps for a composed app.
        let mut pool = wasmtime::PoolingAllocationConfig::default();
        pool.total_component_instances(1000);
        pool.total_core_instances(10_000);
        pool.total_memories(10_000);
        pool.max_memory_size(64 << 20); // 64 MiB per linear memory
        pool.total_tables(10_000);
        wt_config.allocation_strategy(wasmtime::InstanceAllocationStrategy::Pooling(pool));
    }
    let engine = Engine::new(&wt_config)?;

    // shared, process-lifetime state.
    let sqlite_path = args.sqlite_path.clone().unwrap_or_else(kv::SqliteKv::default_path);
    let lattice_mode = args.lattice_nats.is_some();
    let kv_kind = args.kv.clone().unwrap_or_else(|| {
        if lattice_mode { kv::DEFAULT_SHARED.into() } else { "memory".into() }
    });
    let nats_url = args
        .nats_url
        .clone()
        .or_else(|| args.lattice_nats.clone())
        .unwrap_or_else(|| "127.0.0.1:4222".into());
    let kv_backend: Kv = kv::build(&kv_kind, &args.redis_url, &nats_url, &sqlite_path).await?;
    let kv_shared = kv_backend.shared();
    if lattice_mode && !kv_shared {
        // Not fatal: a single-replica app on a node-local store is a legitimate
        // arrangement, and the reconciler refuses the spread case on its own. But it
        // is never what someone means by accident, so it says so.
        eprintln!(
            "comp-host: WARNING --kv {kv_kind} is node-local. A spread stateful app will be \
             refused placement here, and anything running here loses its store if this node \
             does. Use a backend whose store every replica shares (--kv {}).",
            kv::DEFAULT_SHARED
        );
    }
    let cache_backing: CacheBacking = Arc::new(Mutex::new(HashMap::new()));
    let static_dir: Arc<Option<std::path::PathBuf>> =
        Arc::new(args.static_dir.clone().map(std::path::PathBuf::from));

    let addr: SocketAddr = args.addr.parse()?;

    // Who this instance is. A single-app run builds its scope from flags; the
    // lattice will build the same struct from a start command, which is the only
    // difference between the two lanes.
    let limits = Limits {
        mem_cap: args.mem_cap_mb << 20,
        slice_ms: args.slice_ms,
        pool_size: 1,
        allow_private_egress: args.allow_private_egress,
        // This host's own listener is never a legitimate egress target: reaching it
        // would let a component call back in as though it were a client.
        denied_addrs: vec![addr.ip()],
    };
    // File first, flags second, so an explicit `--config` always wins.
    let mut cfg: std::collections::BTreeMap<String, String> = Default::default();
    if let Some(path) = &args.config_file {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading --config-file {path}: {e}"))?;
        cfg.extend(parse_config_file(&text).map_err(|e| anyhow::anyhow!("{path}: {e}"))?);
    }
    cfg.extend(args.configs.iter().cloned());

    let engine = Arc::new(engine);
    let instances: Instances = Arc::new(RwLock::new(HashMap::new()));
    let routes: Routes = Arc::new(RwLock::new(HashMap::new()));

    // The other half of `epoch_interruption`: something has to advance the clock.
    // One task for the whole process, since the epoch is per-Engine.
    {
        let engine = engine.clone();
        tokio::task::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(1));
            loop {
                tick.tick().await;
                engine.increment_epoch();
            }
        });
    }

    // Two lanes, one table. A single-app run puts exactly one instance in it from
    // flags; a lattice node fills it from start commands. Everything downstream —
    // routing, scoping, limits — cannot tell the difference, which is the point.
    match &args.lattice_nats {
        None => {
            let scope: SharedScope = Arc::new(
                StartCommand {
                    tenant: args.tenant.clone(),
                    app: args.app.clone(),
                    component: "root".into(),
                    digest: String::new(),
                    count: 1,
                    config: cfg,
                    links: Default::default(),
                    host_needs: Vec::new(),
                    egress: args.egress.clone(),
                    ingress_host: None,
                }
                .into_scope(&limits),
            );
            let component = Component::from_file(&engine, &args.component)?;
            let pre = ProxyPre::new(build_linker(&engine)?.instantiate_pre(&component)?)?;
            let id = scope.id();
            instances
                .write()
                .unwrap()
                .insert(
                    id.clone(),
                    Arc::new(Instance { scope, pre, remotes: Default::default(), count: 1 }),
                );
            // The catch-all exists ONLY here. A lattice node routes by Host header
            // and 404s on a miss — a fallback there would send one tenant's traffic
            // into another tenant's component on a bad DNS record.
            routes.write().unwrap().insert(CATCH_ALL.into(), id.clone());
            println!("comp-host: serving {} on http://{} as {id}", args.component, addr);
            println!(
                "comp-host: kv backend = {} | allocator = {}",
                kv_kind,
                if args.pool { "pooling" } else { "on-demand" }
            );
        }
        Some(lattice_url) => {
            let nats_url_for_lattice: &str = lattice_url;
            let node = args.node.clone().unwrap_or_else(|| {
                hostname().unwrap_or_else(|| format!("node-{}", std::process::id()))
            });
            // The node's own NATS connection, shared with the agent so wRPC clients
            // ride the same link rather than opening a second one per instance.
            let raw_nats = Arc::new(
                async_nats::connect(nats_url_for_lattice)
                    .await
                    .with_context(|| format!("connecting to NATS at {nats_url_for_lattice}"))?,
            );
            let ag = Arc::new(agent::Agent {
                nats: Some(raw_nats),
                node,
                labels: args.labels.iter().cloned().collect(),
                lattice: args.lattice.clone(),
                engine: engine.clone(),
                kv: kv_backend.clone(),
                cache_backing: cache_backing.clone(),
                instances: instances.clone(),
                routes: routes.clone(),
                limits: limits.clone(),
                state_dir: args.state_dir.clone().map(std::path::PathBuf::from).unwrap_or_else(
                    || std::path::PathBuf::from(state_dir_default()),
                ),
                heartbeat_secs: args.heartbeat_secs,
                address: args.advertise_addr.clone().unwrap_or_else(|| args.addr.clone()),
                kv_shared,
            });
            println!(
                "comp-host: lattice node, listening on http://{addr} | kv = {} ({})",
                kv_kind,
                if kv_shared {
                    "shared — a spread app keeps one store"
                } else {
                    "NODE-LOCAL — this node will not accept a spread stateful app"
                }
            );
            // One implementation today. The agent takes three trait objects and
            // never learns which broker is underneath them.
            let fabric = Arc::new(
                comp_lattice::nats::NatsLattice::connect(
                    nats_url_for_lattice,
                    &args.lattice,
                    std::time::Duration::from_secs(args.heartbeat_secs * 3),
                )
                .await?,
            );
            let fab = agent::Fabric {
                inventory: fabric.clone(),
                commands: fabric.clone(),
                artifacts: fabric,
            };
            tokio::spawn(async move {
                if let Err(e) = agent::run(ag, fab).await {
                    // Deliberately not fatal. Losing the bus degrades cross-node
                    // work; it does not mean this node should stop serving what it
                    // already has (see agent.rs).
                    eprintln!("comp-host: lattice agent stopped: {e:#}");
                }
            });
        }
    }

    println!(
        "comp-host: egress = {}",
        if args.egress.is_empty() {
            "DENY ALL (pass --egress to allow outbound HTTP)".to_string()
        } else {
            args.egress.join(", ")
        }
    );
    if let Some(d) = static_dir.as_ref() {
        println!("comp-host: serving static SPA from {}", d.display());
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let engine = engine.clone();
        let kv_backend = kv_backend.clone();
        let cache_backing = cache_backing.clone();
        let instances = instances.clone();
        let routes = routes.clone();
        let static_dir = static_dir.clone();

        tokio::task::spawn(async move {
            let service = hyper::service::service_fn(move |req| {
                let engine = engine.clone();
                let kv_backend = kv_backend.clone();
                let cache_backing = cache_backing.clone();
                let instances = instances.clone();
                let routes = routes.clone();
                let static_dir = static_dir.clone();
                async move {
                    // static SPA first (GET, non-API). Falls through to the
                    // component for API routes + all non-GET.
                    if let Some(dir) = static_dir.as_ref() {
                        if let Some(resp) = try_static(dir, &req) {
                            return Ok::<_, anyhow::Error>(resp);
                        }
                    }
                    let Some(instance) = resolve(&routes, &instances, &req) else {
                        return Ok(not_found());
                    };
                    handle_request(engine, instance, kv_backend, cache_backing, req).await
                }
            });
            if let Err(e) = http1::Builder::new()
                .serve_connection(io, service)
                .await
            {
                eprintln!("connection error: {e:?}");
            }
        });
    }
}

/// The single-app lane's route key. A lattice node never has one.
const CATCH_ALL: &str = "*";

/// Which instance answers this request.
///
/// The `Host` header, lower-cased, port stripped. A miss is a 404 and **never** a
/// fallback to "the only app" — on a node holding several tenants, a fallback turns
/// one bad DNS record into one tenant's traffic arriving at another tenant's
/// component. The catch-all is inserted only by the single-app lane, where there is
/// exactly one instance and no one to confuse it with.
fn resolve(
    routes: &Routes,
    instances: &Instances,
    req: &hyper::Request<hyper::body::Incoming>,
) -> Option<Arc<Instance>> {
    let host = req
        .headers()
        .get(hyper::header::HOST)
        .and_then(|h| h.to_str().ok())
        .or_else(|| req.uri().host())
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();

    let routes = routes.read().unwrap();
    let id = routes.get(&host).or_else(|| routes.get(CATCH_ALL))?;
    instances.read().unwrap().get(id).cloned()
}

fn not_found() -> hyper::Response<HyperOutgoingBody> {
    use http_body_util::{BodyExt, Full};
    hyper::Response::builder()
        .status(404)
        .header("content-type", "text/plain; charset=utf-8")
        .body(
            Full::new(bytes::Bytes::from_static(b"no application is served at this host\n"))
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .unwrap()
}

/// This node's name on the lattice, when `--node` is not given.
fn hostname() -> Option<String> {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Where a systemd unit's state belongs, with no configuration — the same
/// `StateDirectory=` convention `SqliteKv::default_path` follows.
fn state_dir_default() -> String {
    match std::env::var("STATE_DIRECTORY") {
        Ok(dirs) if !dirs.is_empty() => dirs.split(':').next().unwrap_or(&dirs).to_string(),
        _ => ".comp-state".to_string(),
    }
}

/// API route prefixes that must go to the wasm component, never to static files.
const API_PREFIXES: &[&str] = &[
    "/register", "/login", "/me", "/auth", "/api", "/pets", "/appointments", "/admin", "/i18n",
];

/// Serve a static file from `dir` for a non-API GET, with an index.html SPA
/// fallback (client-side routing). Returns None to let the component handle it
/// (any non-GET, or an API path).
fn try_static(
    dir: &std::path::Path,
    req: &hyper::Request<hyper::body::Incoming>,
) -> Option<hyper::Response<HyperOutgoingBody>> {
    use http_body_util::{BodyExt, Full};
    if req.method() != hyper::Method::GET {
        return None;
    }
    let path = req.uri().path();
    if API_PREFIXES.iter().any(|p| path == *p || path.starts_with(&format!("{p}/")) || path.starts_with(&format!("{p}?"))) {
        return None;
    }
    // resolve a file; "/" -> index.html. Reject path traversal.
    let rel = path.trim_start_matches('/');
    if rel.contains("..") {
        return None;
    }
    let candidate = if rel.is_empty() { dir.join("index.html") } else { dir.join(rel) };
    let (bytes, ctype) = match std::fs::read(&candidate) {
        Ok(b) => (b, content_type(&candidate)),
        // SPA fallback: unknown non-asset path -> index.html (client router).
        Err(_) => {
            let idx = dir.join("index.html");
            match std::fs::read(&idx) {
                Ok(b) => (b, "text/html; charset=utf-8"),
                Err(_) => return None,
            }
        }
    };
    // wasmtime 47's HyperOutgoingBody is an UnsyncBoxBody, not a BoxBody.
    let body = Full::new(bytes::Bytes::from(bytes))
        .map_err(|never| match never {})
        .boxed_unsync();
    Some(
        hyper::Response::builder()
            .status(200)
            .header("content-type", ctype)
            .body(body)
            .unwrap(),
    )
}

fn content_type(p: &std::path::Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

/// Drive one HTTP request through the component's incoming-handler.
async fn handle_request(
    engine: Arc<Engine>,
    instance: Arc<Instance>,
    kv: Kv,
    cache_backing: CacheBacking,
    req: hyper::Request<hyper::body::Incoming>,
) -> Result<hyper::Response<HyperOutgoingBody>> {
    let scope = instance.scope.clone();
    let (mem_cap, slice_ms) = (scope.mem_cap, scope.slice_ms);
    let host = Host {
        table: ResourceTable::new(),
        wasi: WasiCtxBuilder::new().inherit_stderr().build(),
        http: WasiHttpCtx::new(),
        hooks: Egress { scope: scope.clone() },
        kv,
        cache_backing,
        scope,
        limits: wasmtime::StoreLimits::default(),
        rpc: rpc::RpcCtx::new(
            if instance.remotes.is_empty() {
                rpc::Transport::Solo
            } else {
                rpc::Transport::Lattice(instance.remotes.clone())
            },
            Some(std::time::Duration::from_secs(30)),
        ),
    };
    let mut store = Store::new(&engine, host);

    // The `Store` is where a tenant boundary is cheapest to enforce, because one
    // already exists per request. Two limits ride on it:
    //
    //   * linear memory, per app, under the fleet-wide pooling ceiling; and
    //   * a CPU slice, after which the guest YIELDS to the tokio scheduler rather
    //     than trapping. Yielding is the point — a busy component should be slow,
    //     not broken, while its neighbours stay responsive.
    store.limiter(move |h| &mut h.limits);
    store.data_mut().limits = wasmtime::StoreLimitsBuilder::new()
        .memory_size(mem_cap)
        .trap_on_grow_failure(true)
        .build();
    store.set_epoch_deadline(slice_ms);
    store.epoch_deadline_async_yield_and_update(slice_ms);

    let (sender, receiver) = tokio::sync::oneshot::channel();
    // hyper::body::Incoming is already Body<Data=Bytes, Error=hyper::Error>.
    let req = store.data_mut().http().new_incoming_request(
        wasmtime_wasi_http::p2::bindings::http::types::Scheme::Http,
        req,
    )?;
    let out = store.data_mut().http().new_response_outparam(sender)?;
    let proxy = instance.pre.instantiate_async(&mut store).await?;

    let task = tokio::task::spawn(async move {
        proxy
            .wasi_http_incoming_handler()
            .call_handle(&mut store, req, out)
            .await
    });

    match receiver.await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => {
            // the sender was dropped without a response -> the guest trapped.
            let err = task.await.unwrap().unwrap_err();
            Err(anyhow::anyhow!("guest never produced a response: {err:?}"))
        }
    }
}
