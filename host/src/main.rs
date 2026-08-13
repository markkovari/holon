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
mod kvcache;
mod kvprofile;
mod secrets;
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
        //
        // `reveal` is the one import that talks to the network — it fetches the
        // plaintext from the platform on first use (ADR-0051) — so it is the one
        // import that must not block the executor thread it runs on. A named rule
        // REPLACES the default rather than adding to it, so `trappable` is repeated
        // here; and an unmatched rule is a compile error, so a typo cannot silently
        // leave this synchronous.
        imports: {
            default: trappable,
            "comp:secrets/reader.reveal": async | trappable,
        },
        with: {
            // wasmtime >=34 keys a resource as `interface.resource`, not `interface/resource`.
            "wasi:keyvalue/store.bucket": super::HostBucket,
            // The guest holds this and cannot read it (ADR-0051).
            "comp:secrets/reader.secret": super::secrets::SecretHandle,
        },
    });
}

use bindings::cache::store::sink as cache_sink;
use bindings::cache::store::source as cache_source;
use bindings::comp::secrets::reader;
use bindings::comp::store::cas;
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
    /// The HTTP world, when this component exports one.
    ///
    /// `None` for a plug: it exports interfaces other components call, not a door
    /// requests arrive at. Before cross-node serving existed a plug could not start
    /// at all; now it starts, serves its exports over the bus, and simply never
    /// appears in the route table.
    pub(crate) pre: Option<ProxyPre<Host>>,
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
    /// Secrets this instance has already fetched. Dropped — and overwritten — with
    /// the Store, which is per request (ADR-0037).
    secret_cache: secrets::SecretCache,
    /// Where to fetch a granted secret, and the client to do it with.
    platform_url: String,
    fetch_http: reqwest::Client,
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

// ---- comp:store/cas host impl --------------------------------------------
//
// The guard, enforced where the data is (ADR-0065). Both calls take the same
// `HostBucket` resource `wasi:keyvalue` hands out, so the guest still cannot name
// a store it was not given — the ADR-0012 boundary is untouched by this.

impl cas::Host for Host {
    fn get(
        &mut self,
        b: Resource<HostBucket>,
        key: String,
    ) -> wasmtime::Result<Result<Option<cas::Versioned>, store::Error>> {
        let id = self.table.get(&b)?.id.clone();
        Ok(self
            .kv
            .get_revision(&id, &key)
            .map(|o| o.map(|(revision, value)| cas::Versioned { revision, value }))
            .map_err(kv_err))
    }

    fn set(
        &mut self,
        b: Resource<HostBucket>,
        key: String,
        value: Vec<u8>,
        expected: u64,
    ) -> wasmtime::Result<Result<cas::Outcome, store::Error>> {
        let id = self.table.get(&b)?.id.clone();
        Ok(self
            .kv
            .set_if_revision(&id, &key, &value, expected)
            .map(|c| match c {
                kv::Cas::Committed(r) => cas::Outcome::Committed(r),
                kv::Cas::Conflict(r) => cas::Outcome::Conflict(r),
            })
            .map_err(kv_err))
    }
}

// ---- comp:secrets/reader host impl ---------------------------------------
//
// The guest names a KEY; `Scope::secret` is the only way from that string to a
// `SecretRef`, and `SecretRef` cannot be built outside `tenant.rs` (ADR-0051).
// Same shape as buckets and links, for the third time.

impl reader::HostSecret for Host {
    /// The manifest's name for this secret, not the value. The only thing about a
    /// secret that is safe to log, which is why it is the only thing exposed.
    fn key(&mut self, self_: Resource<secrets::SecretHandle>) -> wasmtime::Result<String> {
        Ok(self.table.get(&self_)?.key.clone())
    }

    fn drop(&mut self, rep: Resource<secrets::SecretHandle>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl reader::Host for Host {
    /// Holding a secret. `none` is "not granted", and deliberately not an error:
    /// a component may legitimately run with an optional secret absent.
    fn get(
        &mut self,
        key: String,
    ) -> wasmtime::Result<Result<Option<Resource<secrets::SecretHandle>>, reader::SecretError>> {
        let Some(reference) = self.scope.secret(&key).cloned() else {
            return Ok(Ok(None));
        };
        let res = self.table.push(secrets::SecretHandle { key, reference })?;
        Ok(Ok(Some(res)))
    }

    /// Reading one. The audit point, and the only path to a plaintext.
    ///
    /// The value is fetched on FIRST reveal, then cached for this instance — a
    /// secret on a code path that never runs never enters this process, and an
    /// instance is per-request anyway (ADR-0037), so the cache is short by
    /// construction. `expired` is told apart from `unavailable` because one is a
    /// restart and the other is a manifest problem.
    async fn reveal(
        &mut self,
        s: Resource<secrets::SecretHandle>,
    ) -> wasmtime::Result<Result<String, reader::SecretError>> {
        let handle = self.table.get(&s)?;
        let (key, reference) = (handle.key.clone(), handle.reference.clone());

        // Audited before the value is known, so a failed read is on the record too:
        // "which component tried to read this, and when" is asked after a leak, and
        // the attempts matter as much as the successes. Key and identity only —
        // never a value (ADR-0051).
        eprintln!(
            "{}",
            serde_json::json!({
                "event": "secret.reveal",
                "tenant": self.scope.tenant,
                "app": self.scope.app,
                "component": self.scope.component,
                "key": key,
            })
        );

        if let Some(v) = self.secret_cache.get(&reference) {
            return Ok(Ok(v.clone()));
        }
        match secrets::fetch(
            &self.fetch_http,
            &self.platform_url,
            &self.scope.fetch_token,
            &reference,
        )
        .await
        {
            Ok(v) => {
                self.secret_cache.put(&reference, v.clone());
                Ok(Ok(v))
            }
            Err(e) if e == "expired" => Ok(Err(reader::SecretError::Expired)),
            // Vague on purpose: the guest learns its secret is unavailable, not
            // which of the platform's answers produced that. The detail goes to the
            // node's log, where an operator can act on it.
            Err(e) => {
                eprintln!("comp-host: {} could not read secret {key:?}: {e}", self.scope.id());
                Ok(Err(reader::SecretError::Unavailable("the platform did not supply it".into())))
            }
        }
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
        // Opt-in wire tracing for diagnosing a stalled outbound call. Off unless
        // COMP_TRACE_EGRESS is set, so it costs nothing in a normal run.
        let trace = std::env::var_os("COMP_TRACE_EGRESS").is_some();
        let who = scope.id();
        let handle = wasmtime_wasi::runtime::spawn(async move {
            let target =
                if authority.contains(':') { authority.clone() } else { format!("{authority}:{port}") };
            match tokio::net::lookup_host(&target).await {
                Ok(addrs) => {
                    for a in addrs {
                        if !scope.egress.permits_addr(a) {
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
            if trace {
                eprintln!("comp-host: [egress] {who} dialing {target} (tls={})", config.use_tls);
            }
            // ponytail: the connect re-resolves, so a DNS-rebinding attacker who
            // controls an allow-listed name can still land on a denied address
            // between this check and the dial. The real fix is a connector pinned
            // to the address we checked; do it if egress ever guards something an
            // attacker would spend a rebind on.
            let out = default_send_request_handler(request, config).await;
            if trace {
                match &out {
                    Ok(_) => eprintln!("comp-host: [egress] {who} {target} -> response received"),
                    Err(e) => eprintln!("comp-host: [egress] {who} {target} -> ERROR {e:?}"),
                }
            }
            Ok(out)
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
    /// Where to fetch granted secrets from (ADR-0051). Empty means a component that
    /// asks for one is told its secret is unavailable — which is the right answer
    /// for a node with no platform, and better than a node that silently has none.
    #[arg(long, default_value = "")]
    platform_url: String,
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
    /// NATS URL for `--kv nats`, or a comma-separated list of them.
    ///
    /// List every server in the cluster. A client given one address does learn the
    /// others from the INFO the server sends, and fails over to them — but only
    /// after it has connected to something, so a host starting while its one
    /// listed server is the one that is down cannot bootstrap (ADR-0067).
    ///
    /// Defaults to the lattice's own NATS when `--lattice-nats` is given, because
    /// running a node's store on a different cluster from its control bus is a
    /// thing to do on purpose, not by default.
    #[arg(long)]
    nats_url: Option<String>,
    /// Fall back to wasmtime's on-demand allocator. The POOLING allocator
    /// (pre-reserved instance/memory slots, reused across requests) is the
    /// default because ADR-0054 measured it 21–46% faster at identical idle
    /// memory; this flag exists to reproduce the on-demand baseline.
    #[arg(long = "no-pool", action = clap::ArgAction::SetFalse)]
    pool: bool,
    /// Count and time every store operation, and report on shutdown.
    ///
    /// For answering what a REAL application asks the store for, which is the one
    /// input every caching decision here has been missing — all of them were
    /// measured on a component that writes on every request (ADR-0059). Off the
    /// request path entirely when off: the backend is handed out unwrapped.
    #[arg(long)]
    kv_profile: bool,
    /// Serve repeat reads from a per-node cache for this many milliseconds.
    ///
    /// 0 (the default) is off. ADR-0062 measured 264 reads per write on a real
    /// app, which is why a TTL and no coherence protocol is the trade worth making
    /// — but it IS a trade: a write on another node stays invisible here until the
    /// entry expires. Sound for reads that tolerate a bounded lag, unsound
    /// otherwise, and nothing about the number changes which one an app is.
    #[arg(long, default_value = "0")]
    kv_cache_ms: u64,
    /// How many copies of each `--kv nats` bucket JetStream keeps.
    ///
    /// **0 (the default) means as many as this NATS can hold, up to 3** — it asks
    /// for 3, and falls back to 1 with a warning if the server is not clustered.
    /// So a single-node deployment still works and a clustered one is replicated
    /// without anyone remembering to ask, which is the right way round: one copy
    /// is a total, silent loss the day that disk dies (ADR-0067).
    ///
    /// An explicit number is taken literally and does NOT fall back — asking for
    /// 3 and quietly getting 1 is how you think you are safe when you are not.
    ///
    /// Applies to buckets created from now on. An existing one keeps what it was
    /// made with; `nats stream update` or `DIR=… REPLICAS=3 just restore` moves it.
    #[arg(long, default_value = "0")]
    kv_replicas: usize,

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
pub(crate) fn build_linker(engine: &Engine) -> Result<Linker<Host>> {
    let mut linker: Linker<Host> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
    store::add_to_linker::<_, HasSelf<Host>>(&mut linker, |h| h)?;
    atomics::add_to_linker::<_, HasSelf<Host>>(&mut linker, |h| h)?;
    batch::add_to_linker::<_, HasSelf<Host>>(&mut linker, |h| h)?;
    config::add_to_linker::<_, HasSelf<Host>>(&mut linker, |h| h)?;
    cache_source::add_to_linker::<_, HasSelf<Host>>(&mut linker, |h| h)?;
    cache_sink::add_to_linker::<_, HasSelf<Host>>(&mut linker, |h| h)?;
    reader::add_to_linker::<_, HasSelf<Host>>(&mut linker, |h| h)?;
    cas::add_to_linker::<_, HasSelf<Host>>(&mut linker, |h| h)?;
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
    let kv_backend: Kv =
        kv::build(&kv_kind, &args.redis_url, &nats_url, &sqlite_path, args.kv_replicas).await?;
    // An explicit 1 is a choice, and it is still worth saying out loud once. The
    // automatic path warns from inside `store_for`, where it knows whether the
    // fallback actually happened rather than guessing here.
    if kv_kind == "nats" && args.kv_replicas == 1 {
        eprintln!(
            "comp-host: WARNING --kv-replicas 1 was asked for explicitly. Every bucket \
             this node creates has ONE copy, so losing the server that holds it loses \
             the data. Drop the flag to take as many copies as this NATS can hold."
        );
    }
    // The profiler ends up OUTSIDE the cache, so it keeps counting what the guest
    // asked for rather than what survived the cache. That is what makes a cached
    // run comparable to an uncached one — the demand is the same number in both —
    // and the cache reports its own hit rate separately.
    let cache = (args.kv_cache_ms > 0)
        .then(|| kvcache::CacheKv::wrap(kv_backend.clone(), args.kv_cache_ms));
    let kv_backend: Kv = match &cache {
        Some(c) => c.clone(),
        None => kv_backend,
    };
    // Wrapped, not replaced: `shared()` and every answer come from the real backend,
    // so a profiled run is the same run with a clock on it.
    let profile = args.kv_profile.then(|| kvprofile::ProfileKv::wrap(kv_backend.clone()));
    let kv_backend: Kv = match &profile {
        Some(p) => p.clone(),
        None => kv_backend,
    };
    if let Some(c) = cache.clone() {
        eprintln!(
            "comp-host: keyvalue reads cached for {}ms per node. A write on another \
             node is invisible here until then (ADR-0063).",
            args.kv_cache_ms
        );
        let p = profile.clone();
        tokio::spawn(async move {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            if let Some(p) = p {
                eprintln!("{}", p.report());
            }
            eprintln!("{}", c.report());
            std::process::exit(0);
        });
    } else if let Some(p) = profile.clone() {
        // On the way out, because warm-up and steady state have different mixes and
        // a running total is the honest summary of the whole run. SIGTERM as well as
        // Ctrl-C: every script here stops a host with `kill`.
        tokio::spawn(async move {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            eprintln!("{}", p.report());
            std::process::exit(0);
        });
    }
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
        allow_private_egress: args.allow_private_egress,
        // This host's own listener is never a legitimate egress target: reaching it
        // would let a component call back in as though it were a client. The
        // SOCKET, not the address — everything else on the same machine is the
        // range check's business, and denying the whole IP silently took out any
        // colocated service a component was meant to reach.
        denied_addrs: vec![addr],
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
                    // Solo mode has no platform to fetch from, so a component run
                    // this way is granted nothing rather than being handed a token
                    // that cannot work.
                    secrets: Default::default(),
                    fetch_token: String::new(),
                    host_needs: Vec::new(),
                    egress: args.egress.clone(),
                    ingress_host: None,
                }
                .into_scope(&limits),
            );
            let component = Component::from_file(&engine, &args.component)?;
            let pre = Some(ProxyPre::new(build_linker(&engine)?.instantiate_pre(&component)?)?);
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
            // Every server in the list, for the same reason the store takes a list:
            // failover only helps a process that managed to connect once.
            let lattice_servers = comp_lattice::nats::servers(nats_url_for_lattice);
            let raw_nats = Arc::new(
                async_nats::connect(lattice_servers.clone())
                    .await
                    .with_context(|| {
                        format!("connecting to NATS at {}", lattice_servers.join(", "))
                    })?,
            );
            let ag = Arc::new(agent::Agent {
                platform_url: args.platform_url.clone(),
                compiled: Default::default(),
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

    // Captured once for the serve loop; every store this host builds fetches from
    // the same platform.
    let platform_url = args.platform_url.clone();
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
        let platform_url = platform_url.clone();

        tokio::task::spawn(async move {
            let service = hyper::service::service_fn(move |req| {
                let engine = engine.clone();
                let kv_backend = kv_backend.clone();
                let cache_backing = cache_backing.clone();
                let instances = instances.clone();
                let routes = routes.clone();
                let static_dir = static_dir.clone();
                let platform_url = platform_url.clone();
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
                    handle_request(engine, instance, kv_backend, cache_backing, platform_url, req).await
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
    // A plug is in the instance table but has no HTTP world, so it can never be
    // reached through the door even if something routed to it by mistake.
    instances.read().unwrap().get(id).filter(|i| i.pre.is_some()).cloned()
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


/// One `Store` with this instance's scope, limits and egress policy.
///
/// Factored out because a wRPC-served invocation must get exactly the same store an
/// HTTP request gets — same tenant boundary, same memory cap, same CPU slice, same
/// allow-list. A second construction path would be a second place for one of those
/// to be forgotten, and they are the ones ADR-0023 is about.
pub(crate) fn store_for(
    engine: &Engine,
    scope: SharedScope,
    kv: Kv,
    cache_backing: CacheBacking,
    remotes: std::collections::BTreeMap<String, wrpc_transport_nats::Client>,
    // Where a granted secret is fetched from. Empty in solo mode, where there is no
    // platform and a component is granted nothing anyway.
    platform_url: String,
) -> Store<Host> {
    let (mem_cap, slice_ms) = (scope.mem_cap, scope.slice_ms);
    let host = Host {
        table: ResourceTable::new(),
        wasi: WasiCtxBuilder::new().inherit_stderr().build(),
        http: WasiHttpCtx::new(),
        hooks: Egress { scope: scope.clone() },
        kv,
        cache_backing,
        scope,
        secret_cache: Default::default(),
        platform_url,
        // One client per store is wasteful; reqwest pools internally and a store is
        // per request. ponytail: hoist to a shared client if a profile ever shows it.
        fetch_http: reqwest::Client::new(),
        limits: wasmtime::StoreLimits::default(),
        rpc: rpc::RpcCtx::new(
            if remotes.is_empty() { rpc::Transport::Solo } else { rpc::Transport::Lattice(remotes) },
            // A wrpc call's budget. 30s is right for a store read; it is far too
            // short for a graph where one guest call fans out to a language model
            // and a test suite over several nested wrpc hops. Raised via env for
            // those runs — `data receipt timed out` is this firing.
            Some(std::time::Duration::from_secs(
                std::env::var("COMP_RPC_TIMEOUT_SECS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(30),
            )),
        ),
    };
    let mut store = Store::new(engine, host);
    store.limiter(move |h| &mut h.limits);
    store.data_mut().limits =
        wasmtime::StoreLimitsBuilder::new().memory_size(mem_cap).trap_on_grow_failure(true).build();
    store.set_epoch_deadline(slice_ms);
    store.epoch_deadline_async_yield_and_update(slice_ms);
    store
}

/// Drive one HTTP request through the component's incoming-handler.
async fn handle_request(
    engine: Arc<Engine>,
    instance: Arc<Instance>,
    kv: Kv,
    cache_backing: CacheBacking,
    platform_url: String,
    req: hyper::Request<hyper::body::Incoming>,
) -> Result<hyper::Response<HyperOutgoingBody>> {
    let mut store = store_for(
        &engine,
        instance.scope.clone(),
        kv,
        cache_backing,
        instance.remotes.clone(),
        platform_url,
    );

    let (sender, receiver) = tokio::sync::oneshot::channel();
    // hyper::body::Incoming is already Body<Data=Bytes, Error=hyper::Error>.
    let req = store.data_mut().http().new_incoming_request(
        wasmtime_wasi_http::p2::bindings::http::types::Scheme::Http,
        req,
    )?;
    let out = store.data_mut().http().new_response_outparam(sender)?;
    let Some(pre) = instance.pre.as_ref() else {
        anyhow::bail!("{} serves no HTTP; it is reachable through links only", instance.scope.id())
    };
    let proxy = pre.instantiate_async(&mut store).await?;

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
