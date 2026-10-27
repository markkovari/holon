//! Native wasmtime host for composed components: one `--component`, or every tenant on the node with `--lattice-nats` (ADR-0021, 0023).

mod agent;
mod kv;
mod kvcache;
mod kvprofile;
mod objkv;
mod rpc;
mod secrets;
mod sync;
mod tenant;
// Backend gates live outside `kv.rs`: the file that implements a backend must not judge it.
#[cfg(test)]
mod objkv_test;
#[cfg(test)]
mod surrealkv_test;
#[cfg(test)]
mod turso_test;
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
use wasmtime_wasi_http::p2::{HttpResult, WasiHttpCtxView, WasiHttpHooks, WasiHttpView};
use wasmtime_wasi_http::WasiHttpCtx;

mod bindings {
    wasmtime::component::bindgen!({
        path: "wit",
        world: "host-imports",
        // `trappable` keeps imports returning Result. `reveal` fetches over the network (ADR-0051), so it is async;
        // a named rule replaces the default, hence `trappable` again.
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

pub type Kv = Arc<dyn KvBackend>;
/// `cache:store` backing, one per instance: its WIT has no bucket parameter, so a process-global map lets tenants collide.
pub type CacheBacking = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// One running component; its `InstancePre` is built once at start, not per request.
pub struct Instance {
    pub scope: SharedScope,
    pub cache_backing: CacheBacking,
    /// `None` for a plug: it serves its exports over the bus and has no route.
    pub(crate) pre: Option<HttpPre>,
    /// Remote-import clients, built at start to keep target lookup off the hot path.
    pub remotes: std::collections::BTreeMap<String, wrpc_transport_nats::Client>,
    /// Replicas asked for. One `InstancePre` serves them all; tracked because the reconciler counts per node.
    pub count: u32,
}

/// The HTTP door, p2 `incoming-handler@0.2` or p3 `handler@0.3.0-rc`, picked from the component itself.
#[derive(Clone)]
pub(crate) enum HttpPre {
    P2(ProxyPre<Host>),
    /// Worker pool built on first request: the first point the kv backend and platform url are at hand.
    P3(wasmtime_wasi_http::p3::bindings::ServicePre<Host>, std::sync::OnceLock<P3Handler>),
}

type P3Handler = wasmtime_wasi_http::handler::ProxyHandler<P3Stores>;

/// Same `store_for` as p2, so tenant boundary, caps and egress match. A p3 instance is reused across requests,
/// so its secret cache and memory cap span them (ADR-0037's per-request rule is p2 only).
pub(crate) struct P3Stores {
    engine: Arc<Engine>,
    scope: SharedScope,
    kv: Kv,
    cache_backing: CacheBacking,
    remotes: std::collections::BTreeMap<String, wrpc_transport_nats::Client>,
    platform_url: String,
}

impl wasmtime_wasi_http::handler::HandlerState for P3Stores {
    type StoreData = Host;
    fn new_store(
        &self,
        _req_id: Option<u64>,
    ) -> wasmtime::Result<wasmtime_wasi_http::handler::StoreBundle<Host>> {
        Ok(wasmtime_wasi_http::handler::StoreBundle {
            store: store_for(
                &self.engine,
                self.scope.clone(),
                self.kv.clone(),
                self.cache_backing.clone(),
                self.remotes.clone(),
                self.platform_url.clone(),
            ),
            write_profile: Box::new(|_| {}),
        })
    }
    // ponytail: no per-request wall-clock timeout, as on p2; add one if a hung guest shows up.
    fn request_timeout(&self) -> std::time::Duration {
        std::time::Duration::MAX
    }
    // `wasmtime serve`'s p3 defaults.
    fn idle_instance_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(1)
    }
    fn max_instance_reuse_count(&self) -> usize {
        128
    }
    fn max_instance_concurrent_reuse_count(&self) -> usize {
        16
    }
    fn handle_worker_error(&self, error: wasmtime::Error) {
        eprintln!("comp-host: {} p3 worker: {error:?}", self.scope.id());
    }
}

impl HttpPre {
    pub(crate) fn new(ipre: wasmtime::component::InstancePre<Host>) -> Option<Self> {
        ProxyPre::new(ipre.clone()).map(Self::P2).ok().or_else(|| {
            let pre = wasmtime_wasi_http::p3::bindings::ServicePre::new(ipre).ok()?;
            Some(Self::P3(pre, Default::default()))
        })
    }
}

/// Keyed by `<tenant>/<app>/<component>`; RwLock because every request reads and writes are start-command rare.
// ponytail: RwLock<HashMap>; arc-swap if start/stop ever contends with traffic.
pub type Instances = Arc<RwLock<HashMap<InstanceId, Arc<Instance>>>>;

/// `Host` header -> instance.
pub type Routes = Arc<RwLock<HashMap<String, InstanceId>>>;

/// Carries a `BucketId`, not the guest's name (ADR-0012); on the resource so `atomics` and `batch` inherit it.
pub struct HostBucket {
    id: BucketId,
}

struct Host {
    table: ResourceTable,
    wasi: WasiCtx,
    http: WasiHttpCtx,
    /// Never unrestricted: in a process shared by every tenant, open egress reaches NATS, this listener and the tailnet.
    hooks: Egress,
    kv: Kv,
    cache_backing: CacheBacking,
    /// Built by the host from a control-plane record; never from guest input or `std::env`.
    scope: SharedScope,
    /// Dropped with the Store (ADR-0037).
    secret_cache: secrets::SecretCache,
    platform_url: String,
    fetch_http: reqwest::Client,
    limits: wasmtime::StoreLimits,
    /// `Solo` off a lattice, so the single-app lane needs no broker.
    rpc: rpc::RpcCtx,
}

impl wrpc_runtime_wasmtime::WrpcView for Host {
    type Invoke = rpc::Transport;

    fn wrpc(&mut self) -> wrpc_runtime_wasmtime::WrpcCtxView<'_, Self::Invoke> {
        wrpc_runtime_wasmtime::WrpcCtxView { ctx: &mut self.rpc, table: &mut self.table }
    }
}

fn kv_err(e: anyhow::Error) -> store::Error {
    store::Error::Other(format!("{e:#}"))
}

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
impl wasmtime_wasi_http::p3::WasiHttpView for Host {
    fn http(&mut self) -> wasmtime_wasi_http::p3::WasiHttpCtxView<'_> {
        wasmtime_wasi_http::p3::WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: &mut self.hooks,
        }
    }
}

impl store::Host for Host {
    /// The guest's string is a key into host state, never the bucket itself (ADR-0012).
    /// A miss is `no-such-store`, never a fallback: a fallback hands a guest naming its neighbour's bucket *a* bucket.
    fn open(
        &mut self,
        identifier: String,
    ) -> wasmtime::Result<Result<Resource<HostBucket>, store::Error>> {
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

// One guest call instead of N: the per-key loop is host-side, saving guest<->host round-trips.

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

impl cache_source::Host for Host {
    fn load(&mut self, key: String) -> wasmtime::Result<Result<Option<Vec<u8>>, String>> {
        Ok(Ok(crate::sync::held(&self.cache_backing).get(&key).cloned()))
    }
}
impl cache_sink::Host for Host {
    fn store(&mut self, key: String, value: Vec<u8>) -> wasmtime::Result<Result<(), String>> {
        crate::sync::held(&self.cache_backing).insert(key, value);
        Ok(Ok(()))
    }
    fn remove(&mut self, key: String) -> wasmtime::Result<Result<(), String>> {
        crate::sync::held(&self.cache_backing).remove(&key);
        Ok(Ok(()))
    }
}

impl config::Host for Host {
    fn get(&mut self, key: String) -> wasmtime::Result<Result<Option<String>, config::Error>> {
        Ok(Ok(self.scope.cfg.get(&key).cloned()))
    }
    fn get_all(&mut self) -> wasmtime::Result<Result<Vec<(String, String)>, config::Error>> {
        Ok(Ok(self.scope.cfg.iter().map(|(k, v)| (k.clone(), v.clone())).collect()))
    }
}

// The guard, enforced where the data is (ADR-0065); same `HostBucket` resource, so the ADR-0012 boundary holds.

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

// Stub: only here to satisfy the capability graph.

impl bindings::actor::entity::actor::Host for Host {
    fn spawn(
        &mut self,
        _id: String,
        _state: Vec<u8>,
    ) -> wasmtime::Result<Result<(), bindings::actor::entity::actor::ActorError>> {
        Ok(Err(bindings::actor::entity::actor::ActorError::InternalError(
            "not implemented natively yet".into(),
        )))
    }

    fn send(
        &mut self,
        _id: String,
        _message: Vec<u8>,
    ) -> wasmtime::Result<Result<(), bindings::actor::entity::actor::ActorError>> {
        Ok(Err(bindings::actor::entity::actor::ActorError::InternalError(
            "not implemented natively yet".into(),
        )))
    }

    fn snapshot(
        &mut self,
        _id: String,
    ) -> wasmtime::Result<Result<Vec<u8>, bindings::actor::entity::actor::ActorError>> {
        Ok(Err(bindings::actor::entity::actor::ActorError::InternalError(
            "not implemented natively yet".into(),
        )))
    }
}

// The guest names a key; only `Scope::secret` maps it to a `SecretRef` (ADR-0051).

impl reader::HostSecret for Host {
    /// The manifest's name, never the value: the only safe-to-log part of a secret.
    fn key(&mut self, self_: Resource<secrets::SecretHandle>) -> wasmtime::Result<String> {
        Ok(self.table.get(&self_)?.key.clone())
    }

    fn drop(&mut self, rep: Resource<secrets::SecretHandle>) -> wasmtime::Result<()> {
        self.table.delete(rep)?;
        Ok(())
    }
}

impl reader::Host for Host {
    /// `none` is "not granted", not an error: an optional secret may be absent.
    fn get(
        &mut self,
        key: String,
    ) -> wasmtime::Result<Result<Option<Resource<secrets::SecretHandle>>, reader::SecretError>>
    {
        let Some(reference) = self.scope.secret(&key).cloned() else {
            return Ok(Ok(None));
        };
        let res = self.table.push(secrets::SecretHandle { key, reference })?;
        Ok(Ok(Some(res)))
    }

    /// Fetched on first reveal, then cached: a secret on a path never run never enters this process.
    /// `expired` (restart) is kept apart from `unavailable` (manifest problem).
    async fn reveal(
        &mut self,
        s: Resource<secrets::SecretHandle>,
    ) -> wasmtime::Result<Result<String, reader::SecretError>> {
        let handle = self.table.get(&s)?;
        let (key, reference) = (handle.key.clone(), handle.reference.clone());

        // Audited before the fetch, so failed attempts are on record too; key and identity only, never a value.
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
            // Vague on purpose: the detail goes to the node log, not the guest.
            Err(e) => {
                eprintln!("comp-host: {} could not read secret {key:?}: {e}", self.scope.id());
                Ok(Err(reader::SecretError::Unavailable("the platform did not supply it".into())))
            }
        }
    }
}

/// The name must be allow-listed AND every resolved address outside the denied ranges: a name check alone is
/// beaten by pointing an allow-listed name at `169.254.169.254`.
struct Egress {
    scope: SharedScope,
}

impl WasiHttpHooks for Egress {
    /// The default of 1 deadlocks a guest that writes a many-KB body before calling `handle` (hit at ~8 KB).
    fn outgoing_body_buffer_chunks(&mut self) -> usize {
        8192
    }

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

        // Fail closed before any connection opens.
        if !self.scope.egress.permits_authority(&authority) {
            eprintln!(
                "comp-host: {} denied egress to {authority:?} (not on its allow-list)",
                self.scope.id()
            );
            return Err(ErrorCode::HttpRequestDenied.into());
        }

        let scope = self.scope.clone();
        let port = if config.use_tls { 443 } else { 80 };
        let trace = std::env::var_os("COMP_TRACE_EGRESS").is_some();
        let who = scope.id();
        let handle = wasmtime_wasi::runtime::spawn(async move {
            let target = if authority.contains(':') {
                authority.clone()
            } else {
                format!("{authority}:{port}")
            };
            match resolved_addrs_permitted(&scope, &target).await {
                Ok(()) => {}
                Err(AddrDenied::Prohibited) => return Ok(Err(ErrorCode::DestinationIpProhibited)),
                Err(AddrDenied::Unresolvable) => return Ok(Err(ErrorCode::DestinationUnavailable)),
            }
            if trace {
                eprintln!("comp-host: [egress] {who} dialing {target} (tls={})", config.use_tls);
            }
            // ponytail: the connect re-resolves, so DNS rebinding can still land on a denied address;
            // pin the connector to the checked address if egress ever guards something worth a rebind.
            // Buffered through reqwest, not wasmtime's streaming handler: that stalled against a real TLS remote
            // (headers arrived, body never did). The cost is holding the response in memory.
            let scheme = if config.use_tls { "https" } else { "http" };
            let t0 = std::time::Instant::now();
            let out = reqwest_send(scheme, &authority, request, &config).await;
            if trace {
                let ms = t0.elapsed().as_millis();
                match &out {
                    Ok(r) => eprintln!(
                        "comp-host: [egress] {who} {target} -> {} in {ms}ms",
                        r.resp.status()
                    ),
                    Err(e) => {
                        eprintln!("comp-host: [egress] {who} {target} -> ERROR in {ms}ms {e:?}")
                    }
                }
            }
            Ok(out)
        });
        Ok(HostFutureIncomingResponse::pending(handle))
    }
}

/// p3 egress: same two checks and buffered fetch as p2; must be overridden, upstream's default is unrestricted.
type P3Body = http_body_util::combinators::UnsyncBoxBody<bytes::Bytes, P3ErrorCode>;
type P3ErrorCode = wasmtime_wasi_http::p3::bindings::http::types::ErrorCode;
type P3Io = Box<dyn std::future::Future<Output = Result<(), P3ErrorCode>> + Send>;

impl wasmtime_wasi_http::p3::WasiHttpHooks for Egress {
    fn send_request(
        &mut self,
        request: hyper::Request<P3Body>,
        options: Option<wasmtime_wasi_http::p3::RequestOptions>,
        _fut: P3Io,
    ) -> Box<
        dyn std::future::Future<
                Output = Result<
                    (hyper::Response<P3Body>, P3Io),
                    wasmtime_wasi::TrappableError<P3ErrorCode>,
                >,
            > + Send,
    > {
        use http_body_util::{BodyExt, Full};
        let scope = self.scope.clone();
        Box::new(async move {
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
            if !scope.egress.permits_authority(&authority) {
                eprintln!(
                    "comp-host: {} denied egress to {authority:?} (not on its allow-list)",
                    scope.id()
                );
                return Err(P3ErrorCode::HttpRequestDenied.into());
            }
            let tls = request.uri().scheme_str() != Some("http");
            let target = if authority.contains(':') {
                authority.clone()
            } else {
                format!("{authority}:{}", if tls { 443 } else { 80 })
            };
            match resolved_addrs_permitted(&scope, &target).await {
                Ok(()) => {}
                Err(AddrDenied::Prohibited) => {
                    return Err(P3ErrorCode::DestinationIpProhibited.into())
                }
                Err(AddrDenied::Unresolvable) => {
                    return Err(P3ErrorCode::DestinationUnavailable.into())
                }
            }
            let (parts, body) = request.into_parts();
            let body = body.collect().await.map_err(|_| P3ErrorCode::HttpRequestBodySize(None))?;
            // wasmtime's p2 defaults, for a guest that set no timeouts.
            let default = std::time::Duration::from_secs(600);
            let opts = options.unwrap_or_default();
            let resp = reqwest_fetch(
                if tls { "https" } else { "http" },
                &authority,
                parts,
                body.to_bytes(),
                opts.connect_timeout.unwrap_or(default),
                opts.first_byte_timeout.unwrap_or(default),
            )
            .await
            .map_err(|e| P3ErrorCode::InternalError(Some(e)))?
            .map(|b| Full::new(b).map_err(|e: std::convert::Infallible| match e {}).boxed_unsync());
            Ok((resp, Box::new(async { Ok(()) }) as P3Io))
        })
    }
}

/// Buffered fully by reqwest, so the guest gets a finished body with no background worker to stall.
async fn reqwest_send(
    scheme: &str,
    authority: &str,
    request: hyper::Request<HyperOutgoingBody>,
    config: &OutgoingRequestConfig,
) -> Result<wasmtime_wasi_http::p2::types::IncomingResponse, ErrorCode> {
    use http_body_util::{BodyExt, Full};

    let (parts, body) = request.into_parts();
    let body_bytes = body
        .collect()
        .await
        .map(|c| c.to_bytes())
        .map_err(|_| ErrorCode::HttpRequestBodySize(None))?;
    let resp = reqwest_fetch(
        scheme,
        authority,
        parts,
        body_bytes,
        config.connect_timeout,
        config.first_byte_timeout,
    )
    .await
    .map_err(|e| ErrorCode::InternalError(Some(e)))?
    .map(|bytes| Full::new(bytes).map_err(|e: std::convert::Infallible| match e {}).boxed_unsync());

    Ok(wasmtime_wasi_http::p2::types::IncomingResponse {
        resp,
        worker: None,
        between_bytes_timeout: config.between_bytes_timeout,
    })
}

enum AddrDenied {
    Prohibited,
    Unresolvable,
}

/// Every resolved address must be outside the denied ranges; shared by p2 and p3 so the check exists once.
async fn resolved_addrs_permitted(scope: &SharedScope, target: &str) -> Result<(), AddrDenied> {
    let addrs = tokio::net::lookup_host(target).await.map_err(|_| AddrDenied::Unresolvable)?;
    for a in addrs {
        if !scope.egress.permits_addr(a) {
            eprintln!(
                "comp-host: {} denied egress to {target} — it resolves to {}",
                scope.id(),
                a.ip()
            );
            return Err(AddrDenied::Prohibited);
        }
    }
    Ok(())
}

async fn reqwest_fetch(
    scheme: &str,
    authority: &str,
    parts: hyper::http::request::Parts,
    body_bytes: bytes::Bytes,
    connect_timeout: std::time::Duration,
    first_byte_timeout: std::time::Duration,
) -> Result<hyper::Response<bytes::Bytes>, String> {
    let path = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    let url = format!("{scheme}://{authority}{path}");

    // Skip headers reqwest owns: host/content-length come from URL and body, hop-by-hop from its connection.
    let mut headers = reqwest::header::HeaderMap::new();
    for (k, v) in parts.headers.iter() {
        let name = k.as_str().to_ascii_lowercase();
        if matches!(
            name.as_str(),
            "host"
                | "content-length"
                | "connection"
                | "transfer-encoding"
                | "keep-alive"
                | "upgrade"
        ) {
            continue;
        }
        headers.insert(k.clone(), v.clone());
    }

    let client = reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .timeout(first_byte_timeout)
        .build()
        .map_err(|_| "http client".to_string())?;

    let resp = client
        .request(parts.method, &url)
        .headers(headers)
        .body(body_bytes)
        .send()
        .await
        .map_err(|e| format!("reqwest: {e}"))?;

    let status = resp.status();
    let resp_headers = resp.headers().clone();
    let bytes = resp.bytes().await.map_err(|e| format!("reqwest body: {e}"))?;

    let mut builder = hyper::Response::builder().status(status);
    for (k, v) in resp_headers.iter() {
        // The guest gets a fixed in-memory body, so hyper recomputes framing.
        let name = k.as_str().to_ascii_lowercase();
        if matches!(name.as_str(), "content-length" | "transfer-encoding" | "connection") {
            continue;
        }
        builder = builder.header(k, v);
    }
    builder.body(bytes).map_err(|_| "response build".to_string())
}

// No process-environment `wasi:config`: in a shared process that is a cross-tenant read.

#[derive(Parser)]
#[command(name = "comp-host", about = "Run the composed vet-domain wasm over wasmtime")]
struct Args {
    /// Path to the composed component wasm.
    #[arg(long, default_value = "../components/target/vet_domain.composed.wasm")]
    component: String,
    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1:3007")]
    addr: String,
    /// Where to fetch granted secrets from (ADR-0051). Empty: every secret is unavailable.
    #[arg(long, default_value = "")]
    platform_url: String,
    /// Optional directory of static files (a built SPA) to serve for GET
    /// requests that aren't API routes. Omit for API-only.
    #[arg(long)]
    static_dir: Option<String>,
    /// Key-value backend: memory | sqlite | redis | nats | surreal | turso.
    /// Defaults to `nats` on a lattice node (the only store replicas share, ADR-0027) and `memory` otherwise.
    #[arg(long)]
    kv: Option<String>,
    /// Redis URL for --kv redis.
    #[arg(long, default_value = "redis://127.0.0.1:6379")]
    redis_url: String,
    /// File for `--kv sqlite` (default `$STATE_DIRECTORY/kv.db`, else ./comp-kv.db);
    /// also the HTTP endpoint for `--kv surreal` / `--kv turso`.
    #[arg(long)]
    sqlite_path: Option<String>,
    /// Auth token FILE for `--kv surreal` or `--kv turso` (required for hosted Turso).
    #[arg(long)]
    kv_token: Option<String>,

    /// Where object bytes live: unset keeps them on `--kv`; `s3` uses an S3-compatible bucket.
    /// The store must enforce conditional writes (checked at startup).
    #[arg(long)]
    blob: Option<String>,
    /// S3 API endpoint for `--blob s3`, e.g.
    /// `https://<account>.r2.cloudflarestorage.com` or `http://127.0.0.1:9000`.
    #[arg(long)]
    s3_endpoint: Option<String>,
    /// The one S3 bucket every tenant's objects go in, under its own prefix.
    #[arg(long, default_value = "holon-blobs")]
    s3_bucket: String,
    /// Signing region. R2 takes `auto`; RustFS and MinIO take `us-east-1`.
    #[arg(long, default_value = "us-east-1")]
    s3_region: String,
    /// Access key id / secret files (not flags, so they stay out of `ps`); else `S3_ACCESS_KEY` / `S3_SECRET_KEY`.
    #[arg(long)]
    s3_access_key_file: Option<String>,
    #[arg(long)]
    s3_secret_key_file: Option<String>,
    /// Path-style addressing (`host/bucket/key`). RustFS and MinIO need it.
    #[arg(long)]
    s3_path_style: bool,
    /// NATS URL for `--kv nats`, or a comma-separated list of them.
    ///
    /// List every server: a client learns the others only after connecting once (ADR-0067).
    /// Defaults to the `--lattice-nats` URL.
    #[arg(long)]
    nats_url: Option<String>,
    /// Use wasmtime's on-demand allocator; pooling is the default (ADR-0054: 21–46% faster).
    #[arg(long = "no-pool", action = clap::ArgAction::SetFalse)]
    pool: bool,
    /// Count and time every store operation, and report on shutdown.
    #[arg(long)]
    kv_profile: bool,
    /// Serve repeat reads from a per-node cache for this many milliseconds.
    ///
    /// 0 = off. A write on another node stays invisible here until the entry expires (ADR-0062).
    #[arg(long, default_value = "0")]
    kv_cache_ms: u64,
    /// How many copies of each `--kv nats` bucket JetStream keeps.
    ///
    /// 0 asks for 3 and falls back to 1 if NATS is not clustered; an explicit number never falls back (ADR-0067).
    /// Applies to new buckets only.
    #[arg(long, default_value = "0")]
    kv_replicas: usize,

    /// Tenant; with `--app` it decides the store `open("default")` resolves to.
    #[arg(long, default_value = "local")]
    tenant: String,
    /// Application name. See `--tenant`.
    #[arg(long, default_value = "app")]
    app: String,
    /// Config for `wasi:config`, repeatable: `--config grace-period-secs=5`.
    #[arg(long = "config", value_parser = parse_kv)]
    configs: Vec<(String, String)>,
    /// A file of `key = value` lines to seed config from, applied before `--config`.
    #[arg(long)]
    config_file: Option<String>,
    /// Authorities this component may reach over `wasi:http`, repeatable; none means no outbound HTTP.
    /// `--egress '*'` skips the name check, never the address one.
    #[arg(long = "egress")]
    egress: Vec<String>,
    /// Let a component reach private networks (loopback, RFC1918, link-local, Tailscale CGNAT).
    /// Development only: on a node those are the NATS bus, cloud metadata and every other node.
    #[arg(long)]
    allow_private_egress: bool,
    /// Per-instance linear memory ceiling, in MiB.
    #[arg(long, default_value = "64")]
    mem_cap_mb: usize,
    /// How long a guest may run before it is made to yield, in milliseconds.
    /// Fairness, not a request timeout.
    #[arg(long, default_value = "50")]
    slice_ms: u64,

    /// Join a lattice at this NATS URL and take instances from start commands instead of `--component`.
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
    /// Where artifacts and the instance ledger live. Defaults to `$STATE_DIRECTORY`.
    #[arg(long)]
    state_dir: Option<String>,
    /// Seconds between inventory publications. The bucket expires an entry after
    /// three of these, which is how a departed node disappears.
    #[arg(long, default_value = "5")]
    heartbeat_secs: u64,
    /// Where an ingress can reach this node, `host:port`. Defaults to `--addr`; set it when bound to `0.0.0.0`.
    #[arg(long)]
    advertise_addr: Option<String>,
}

fn parse_kv(s: &str) -> std::result::Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.trim().to_string(), v.to_string()))
        .ok_or_else(|| format!("expected key=value, got {s:?}"))
}

/// `key = value` lines, `#` comments. Not TOML: `wasi:config` values are all strings.
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

/// The one place host capabilities are granted; must agree with `agent::HOST_IFACES`.
pub(crate) fn build_linker(engine: &Engine) -> Result<Linker<Host>> {
    let mut linker: Linker<Host> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)?;
    // p3 beside p2: distinct package versions, so one linker serves both and a p3 guest's `wasi:cli@0.2`.
    wasmtime_wasi_http::p3::add_to_linker(&mut linker)?;
    wasmtime_wasi::p3::clocks::add_to_linker(&mut linker)?;
    wasmtime_wasi::p3::random::add_to_linker(&mut linker)?;
    wasmtime_wasi::p3::cli::add_to_linker(&mut linker)?;
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

/// Secrets come from files or the environment, never a flag.
fn s3_config(args: &Args) -> Result<objkv::S3Config> {
    let secret = |file: &Option<String>, env: &str| -> Result<String> {
        match file {
            Some(p) => Ok(std::fs::read_to_string(p)
                .with_context(|| format!("reading {p}"))?
                .trim()
                .to_string()),
            None => std::env::var(env).with_context(|| {
                format!(
                    "--blob s3 needs --s3-{}-file or {env}",
                    if env.contains("ACCESS") { "access-key" } else { "secret-key" }
                )
            }),
        }
    };
    Ok(objkv::S3Config {
        endpoint: args.s3_endpoint.clone().context("--blob s3 needs --s3-endpoint")?,
        bucket: args.s3_bucket.clone(),
        region: args.s3_region.clone(),
        access_key: secret(&args.s3_access_key_file, "S3_ACCESS_KEY")?,
        secret_key: secret(&args.s3_secret_key_file, "S3_SECRET_KEY")?,
        path_style: args.s3_path_style,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let mut wt_config = Config::new();
    // Make a non-yielding guest yield: fairness on a shared box. Epoch, not fuel: fuel is for billing.
    wt_config.epoch_interruption(true);
    // The async component ABI — what a p3 guest's `handler@0.3` export is lifted with.
    wt_config.wasm_component_model_async(true);
    if args.pool {
        // Pooling: instantiating the component graph per request costs a slot grab, not fresh mmaps.
        let mut pool = wasmtime::PoolingAllocationConfig::default();
        pool.total_component_instances(1000);
        pool.total_core_instances(10_000);
        pool.total_memories(10_000);
        pool.max_memory_size(64 << 20); // 64 MiB per linear memory
        pool.total_tables(10_000);
        wt_config.allocation_strategy(wasmtime::InstanceAllocationStrategy::Pooling(pool));
    }
    let engine = Engine::new(&wt_config)?;

    let sqlite_path = args.sqlite_path.clone().unwrap_or_else(kv::SqliteKv::default_path);
    let lattice_mode = args.lattice_nats.is_some();
    let kv_kind = args.kv.clone().unwrap_or_else(|| {
        if lattice_mode {
            kv::DEFAULT_SHARED.into()
        } else {
            "memory".into()
        }
    });
    let nats_url = args
        .nats_url
        .clone()
        .or_else(|| args.lattice_nats.clone())
        .unwrap_or_else(|| "127.0.0.1:4222".into());
    let kv_token = args
        .kv_token
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let kv_backend: Kv =
        kv::build(&kv_kind, &args.redis_url, &nats_url, &sqlite_path, args.kv_replicas, &kv_token)
            .await?;
    // Explicit 1 warns here; the automatic fallback warns in `store_for`, where it knows it happened.
    if kv_kind == "nats" && args.kv_replicas == 1 {
        eprintln!(
            "comp-host: WARNING --kv-replicas 1 was asked for explicitly. Every bucket \
             this node creates has ONE copy, so losing the server that holds it loses \
             the data. Drop the flag to take as many copies as this NATS can hold."
        );
    }
    // Profiler outside the cache: it counts guest demand, comparable across cached and uncached runs.
    let cache = (args.kv_cache_ms > 0)
        .then(|| kvcache::CacheKv::wrap(kv_backend.clone(), args.kv_cache_ms));
    let kv_backend: Kv = match &cache {
        Some(c) => c.clone(),
        None => kv_backend,
    };
    // After the cache, so object bytes are never held in it; before the
    // profiler, so it still counts everything the guest asked for.
    let kv_backend: Kv = match args.blob.as_deref() {
        None => kv_backend,
        Some("s3") => {
            let cfg = s3_config(&args)?;
            let objects = objkv::S3Kv::connect(&cfg)?;
            eprintln!(
                "comp-host: object bytes on S3 bucket {} at {} (conditional writes verified)",
                cfg.bucket, cfg.endpoint
            );
            objkv::RoutedKv::new(kv_backend, Arc::new(objects))
        }
        Some(other) => anyhow::bail!("unknown --blob backend: {other} (use s3, or leave it unset)"),
    };
    // Wrapped, not replaced: a profiled run is the same run with a clock on it.
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
        // Report on exit, SIGTERM too: every script here stops a host with `kill`.
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
        // Not fatal: the reconciler refuses the spread case itself.
        eprintln!(
            "comp-host: WARNING --kv {kv_kind} is node-local. A spread stateful app will be \
             refused placement here, and anything running here loses its store if this node \
             does. Use a backend whose store every replica shares (--kv {}).",
            kv::DEFAULT_SHARED
        );
    }
    let static_dir: Arc<Option<std::path::PathBuf>> =
        Arc::new(args.static_dir.clone().map(std::path::PathBuf::from));

    let addr: SocketAddr = args.addr.parse()?;

    let limits = Limits {
        mem_cap: args.mem_cap_mb << 20,
        slice_ms: args.slice_ms,
        allow_private_egress: args.allow_private_egress,
        // Never this host's own listener; the socket, not the IP, so colocated services stay reachable.
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

    // The other half of `epoch_interruption`: one ticker per process, since the epoch is per-Engine.
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

    // Two lanes, one table: routing, scoping and limits cannot tell a solo run from a lattice node.
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
                    // Solo mode has no platform, so nothing is granted.
                    secrets: Default::default(),
                    fetch_token: String::new(),
                    host_needs: Vec::new(),
                    egress: args.egress.clone(),
                    ingress_host: None,
                }
                .into_scope(&limits),
            );
            let component = Component::from_file(&engine, &args.component)?;
            let pre = Some(
                HttpPre::new(build_linker(&engine)?.instantiate_pre(&component)?)
                    .context("component exports neither wasi:http/incoming-handler nor handler")?,
            );
            let id = scope.id();
            instances.write().unwrap().insert(
                id.clone(),
                Arc::new(Instance {
                    scope,
                    cache_backing: Arc::new(Mutex::new(HashMap::new())),
                    pre,
                    remotes: Default::default(),
                    count: 1,
                }),
            );
            // Catch-all only in solo mode: on a lattice a fallback would route one tenant's traffic to another.
            crate::sync::writing(&routes).insert(CATCH_ALL.into(), id.clone());
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
            // Shared with the agent so wRPC rides one connection; every server listed, since failover needs one connect.
            let lattice_servers = comp_lattice::nats::servers(nats_url_for_lattice);
            let raw_nats =
                Arc::new(async_nats::connect(lattice_servers.clone()).await.with_context(
                    || format!("connecting to NATS at {}", lattice_servers.join(", ")),
                )?);
            let ag = Arc::new(agent::Agent {
                platform_url: args.platform_url.clone(),
                compiled: Default::default(),
                nats: Some(raw_nats),
                node,
                labels: args.labels.iter().cloned().collect(),
                lattice: args.lattice.clone(),
                engine: engine.clone(),
                kv: kv_backend.clone(),
                instances: instances.clone(),
                routes: routes.clone(),
                limits: limits.clone(),
                state_dir: args
                    .state_dir
                    .clone()
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| std::path::PathBuf::from(state_dir_default())),
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
                    // Not fatal: losing the bus must not stop serving what this node already has.
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

    let platform_url = args.platform_url.clone();
    let listener = tokio::net::TcpListener::bind(addr).await?;

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let engine = engine.clone();
        let kv_backend = kv_backend.clone();
        let instances = instances.clone();
        let routes = routes.clone();
        let static_dir = static_dir.clone();
        let platform_url = platform_url.clone();

        tokio::task::spawn(async move {
            let service = hyper::service::service_fn(move |req| {
                let engine = engine.clone();
                let kv_backend = kv_backend.clone();
                let instances = instances.clone();
                let routes = routes.clone();
                let static_dir = static_dir.clone();
                let platform_url = platform_url.clone();
                async move {
                    if let Some(dir) = static_dir.as_ref() {
                        if let Some(resp) = try_static(dir, &req) {
                            return Ok::<_, anyhow::Error>(resp);
                        }
                    }
                    let Some(instance) = resolve(&routes, &instances, &req) else {
                        return Ok(not_found());
                    };
                    handle_request(engine, instance, kv_backend, platform_url, req).await
                }
            });
            if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                eprintln!("connection error: {e:?}");
            }
        });
    }
}

const CATCH_ALL: &str = "*";

/// By `Host` header; a miss is a 404, never a fallback (the catch-all exists only in solo mode).
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

    let routes = crate::sync::reading(routes);
    let id = routes.get(&host).or_else(|| routes.get(CATCH_ALL))?;
    // A plug has no HTTP world, so it is never reachable through the door.
    crate::sync::reading(instances).get(id).filter(|i| i.pre.is_some()).cloned()
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

fn hostname() -> Option<String> {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn state_dir_default() -> String {
    match std::env::var("STATE_DIRECTORY") {
        Ok(dirs) if !dirs.is_empty() => dirs.split(':').next().unwrap_or(&dirs).to_string(),
        _ => ".comp-state".to_string(),
    }
}

const API_PREFIXES: &[&str] =
    &["/register", "/login", "/me", "/auth", "/api", "/pets", "/appointments", "/admin", "/i18n"];

/// SPA fallback to index.html; `None` hands the request to the component.
fn try_static(
    dir: &std::path::Path,
    req: &hyper::Request<hyper::body::Incoming>,
) -> Option<hyper::Response<HyperOutgoingBody>> {
    use http_body_util::{BodyExt, Full};
    if req.method() != hyper::Method::GET {
        return None;
    }
    let path = req.uri().path();
    if API_PREFIXES.iter().any(|p| {
        path == *p || path.starts_with(&format!("{p}/")) || path.starts_with(&format!("{p}?"))
    }) {
        return None;
    }
    let rel = path.trim_start_matches('/');
    if rel.contains("..") {
        return None;
    }
    let candidate = if rel.is_empty() { dir.join("index.html") } else { dir.join(rel) };
    let (bytes, ctype) = match std::fs::read(&candidate) {
        Ok(b) => (b, content_type(&candidate)),
        Err(_) => {
            let idx = dir.join("index.html");
            match std::fs::read(&idx) {
                Ok(b) => (b, "text/html; charset=utf-8"),
                Err(_) => return None,
            }
        }
    };
    let body = Full::new(bytes::Bytes::from(bytes)).map_err(|never| match never {}).boxed_unsync();
    Some(hyper::Response::builder().status(200).header("content-type", ctype).body(body).unwrap())
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

/// Shared by the HTTP and wRPC paths so the tenant boundary, caps and allow-list cannot diverge (ADR-0023).
pub(crate) fn store_for(
    engine: &Engine,
    scope: SharedScope,
    kv: Kv,
    cache_backing: CacheBacking,
    remotes: std::collections::BTreeMap<String, wrpc_transport_nats::Client>,
    // Empty in solo mode, where nothing is granted.
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
        // ponytail: a client per store (per request); hoist to a shared one if a profile ever shows it.
        fetch_http: reqwest::Client::new(),
        limits: wasmtime::StoreLimits::default(),
        rpc: rpc::RpcCtx::new(
            if remotes.is_empty() {
                rpc::Transport::Solo
            } else {
                rpc::Transport::Lattice(remotes)
            },
            // 30s suits a store read; deep LLM/test fan-outs raise it (`data receipt timed out` is this firing).
            Some(std::time::Duration::from_secs(
                std::env::var("HOLON_RPC_TIMEOUT_SECS")
                    .or_else(|_| std::env::var("COMP_RPC_TIMEOUT_SECS"))
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

async fn handle_request(
    engine: Arc<Engine>,
    instance: Arc<Instance>,
    kv: Kv,
    platform_url: String,
    req: hyper::Request<hyper::body::Incoming>,
) -> Result<hyper::Response<HyperOutgoingBody>> {
    let pre = match instance.pre.as_ref() {
        None => anyhow::bail!(
            "{} serves no HTTP; it is reachable through links only",
            instance.scope.id()
        ),
        Some(HttpPre::P3(pre, handler)) => {
            let handler = handler.get_or_init(|| {
                wasmtime_wasi_http::handler::ProxyHandler::new(
                    P3Stores {
                        engine: engine.clone(),
                        scope: instance.scope.clone(),
                        kv,
                        cache_backing: instance.cache_backing.clone(),
                        remotes: instance.remotes.clone(),
                        platform_url,
                    },
                    wasmtime_wasi_http::handler::ProxyPre::P3(pre.clone()),
                )
            });
            return handle_request_p3(handler, req).await;
        }
        Some(HttpPre::P2(pre)) => pre,
    };
    let mut store = store_for(
        &engine,
        instance.scope.clone(),
        kv,
        instance.cache_backing.clone(),
        instance.remotes.clone(),
        platform_url,
    );

    let (sender, receiver) = tokio::sync::oneshot::channel();
    let req = WasiHttpView::http(store.data_mut())
        .new_incoming_request(wasmtime_wasi_http::p2::bindings::http::types::Scheme::Http, req)?;
    let out = WasiHttpView::http(store.data_mut()).new_response_outparam(sender)?;
    let proxy = pre.instantiate_async(&mut store).await?;

    let task = tokio::task::spawn(async move {
        proxy.wasi_http_incoming_handler().call_handle(&mut store, req, out).await
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

/// p3: a request is a task on a live worker instance; the store outlives the response,
/// since the guest keeps writing its body after `handle` returns.
async fn handle_request_p3(
    handler: &P3Handler,
    req: hyper::Request<hyper::body::Incoming>,
) -> Result<hyper::Response<HyperOutgoingBody>> {
    use http_body_util::BodyExt;
    use wasmtime_wasi_http::handler::Proxy;
    use wasmtime_wasi_http::p3::bindings::http::types::ErrorCode as P3Error;
    use wasmtime_wasi_http::p3::Request;

    let (tx, rx) = tokio::sync::oneshot::channel();
    let (err_tx, err_rx) = tokio::sync::oneshot::channel::<wasmtime::Error>();
    handler.spawn(
        None,
        Box::new(move |acc, proxy| {
            Box::pin(async move {
                let Proxy::P3(service) = proxy else { unreachable!("P3 handler") };
                let run = async {
                    let req = req.map(|b| b.map_err(P3Error::from_hyper_request_error));
                    let (request, io) = Request::from_http(req);
                    let res = service
                        .handle(acc, request)
                        .await?
                        .map_err(|e| wasmtime::format_err!("guest returned error-code {e:?}"))?;
                    let res = acc.with(|mut s| res.into_http(&mut s, io))?;
                    // Dropped by hyper once the body is fully sent — until then this
                    // task, and so the instance the body streams out of, stays busy.
                    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
                    let res = res.map(|body| {
                        DropSignal { body: body.boxed_unsync(), _done: done_tx }.boxed_unsync()
                    });
                    if tx.send(res).is_ok() {
                        let _ = done_rx.await;
                    }
                    wasmtime::Result::<()>::Ok(())
                };
                if let Err(e) = run.await {
                    let _ = err_tx.send(e);
                }
            })
        }),
    );
    match rx.await {
        Ok(res) => Ok(res.map(|b| {
            b.map_err(|e| {
                wasmtime_wasi_http::p2::bindings::http::types::ErrorCode::InternalError(Some(
                    format!("{e:?}"),
                ))
            })
            .boxed_unsync()
        })),
        Err(_) => Err(anyhow::anyhow!("guest never produced a response: {:?}", err_rx.await.ok())),
    }
}

/// A body that signals, by being dropped, that hyper is done with it.
struct DropSignal<B> {
    body: B,
    _done: tokio::sync::oneshot::Sender<()>,
}

impl<B: hyper::body::Body + Unpin> hyper::body::Body for DropSignal<B> {
    type Data = B::Data;
    type Error = B::Error;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<B::Data>, B::Error>>> {
        std::pin::Pin::new(&mut self.body).poll_frame(cx)
    }
    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }
    fn size_hint(&self) -> hyper::body::SizeHint {
        self.body.size_hint()
    }
}
