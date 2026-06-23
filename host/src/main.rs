//! `vet-host` — a NATIVE Rust server that runs the composed `vet-domain` wasm
//! component over wasmtime. No Node, no wasmCloud: this binary IS the host.
//!
//! It loads `vet_domain.composed.wasm` (the whole vet-clinic backend as one
//! component — vet-domain + auth-guard + records + validate + search), serves
//! its `wasi:http/incoming-handler` export over a hyper TCP listener, and
//! satisfies the component's imports host-side:
//!   - standard WASI (cli/clocks/random/io/filesystem) via wasmtime-wasi
//!   - wasi:http via wasmtime-wasi-http
//!   - wasi:keyvalue@0.2.0-draft  -> an in-memory store implemented here
//!   - wasi:config@0.2.0-draft    -> the process environment (VET_* keys)
//!
//! The SAME .wasm runs under jco (examples/jco-vet-domain) and on wasmCloud;
//! this is a third host, proving the component is host-agnostic. Swap the
//! in-memory KV for redis/sqlite/NATS and the component is unchanged.

mod kv;
use kv::KvBackend;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use clap::Parser;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use wasmtime::component::{Component, Linker, Resource, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiView};
use wasmtime_wasi_http::bindings::ProxyPre;
use wasmtime_wasi_http::body::HyperOutgoingBody;
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpView};

// Generate host traits for the non-standard imports from host/wit/host.wit.
mod bindings {
    wasmtime::component::bindgen!({
        path: "wit",
        world: "host-imports",
        async: false,
        trappable_imports: true,
        with: {
            "wasi:keyvalue/store/bucket": super::HostBucket,
        },
    });
}

use bindings::cache::store::sink as cache_sink;
use bindings::cache::store::source as cache_source;
use bindings::wasi::config::runtime as config;
use bindings::wasi::keyvalue::atomics;
use bindings::wasi::keyvalue::store;

// ---- the key-value store -------------------------------------------------
// The guest's wasi:keyvalue is backed by a swappable `KvBackend` (memory /
// redis / nats — chosen by `--kv`). The component bytes never change; only this
// host-side impl does. (See kv.rs.)

type Kv = Arc<dyn KvBackend>;
/// the cache component's backing store (flat key -> bytes), shares the same Kv
/// under a reserved bucket.
type CacheBacking = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// A host resource handed to the guest when it calls `store.open(name)`.
pub struct HostBucket {
    name: String,
}

// ---- the per-request store state -----------------------------------------

struct Host {
    table: ResourceTable,
    wasi: WasiCtx,
    http: WasiHttpCtx,
    kv: Kv,
    cache_backing: CacheBacking,
    config: Arc<HashMap<String, String>>,
}

/// Map a backend error to the wasi:keyvalue `error` variant.
fn kv_err(e: anyhow::Error) -> store::Error {
    store::Error::Other(format!("{e:#}"))
}

impl WasiView for Host {
    fn ctx(&mut self) -> &mut WasiCtx {
        &mut self.wasi
    }
    fn table(&mut self) -> &mut ResourceTable {
        &mut self.table
    }
}
impl WasiHttpView for Host {
    fn ctx(&mut self) -> &mut WasiHttpCtx {
        &mut self.http
    }
    fn table(&mut self) -> &mut ResourceTable {
        &mut self.table
    }
}

// ---- wasi:keyvalue/store host impl ---------------------------------------

impl store::Host for Host {
    fn open(&mut self, identifier: String) -> Result<Result<Resource<HostBucket>, store::Error>> {
        // a bucket handle is just the name; the backend lazily creates it.
        let res = self.table.push(HostBucket { name: identifier })?;
        Ok(Ok(res))
    }
}

impl store::HostBucket for Host {
    fn get(
        &mut self,
        self_: Resource<HostBucket>,
        key: String,
    ) -> Result<Result<Option<Vec<u8>>, store::Error>> {
        let name = self.table.get(&self_)?.name.clone();
        Ok(self.kv.get(&name, &key).map_err(kv_err))
    }

    fn set(
        &mut self,
        self_: Resource<HostBucket>,
        key: String,
        value: Vec<u8>,
    ) -> Result<Result<(), store::Error>> {
        let name = self.table.get(&self_)?.name.clone();
        Ok(self.kv.set(&name, &key, &value).map_err(kv_err))
    }

    fn delete(
        &mut self,
        self_: Resource<HostBucket>,
        key: String,
    ) -> Result<Result<(), store::Error>> {
        let name = self.table.get(&self_)?.name.clone();
        Ok(self.kv.delete(&name, &key).map_err(kv_err))
    }

    fn exists(
        &mut self,
        self_: Resource<HostBucket>,
        key: String,
    ) -> Result<Result<bool, store::Error>> {
        let name = self.table.get(&self_)?.name.clone();
        Ok(self.kv.exists(&name, &key).map_err(kv_err))
    }

    fn list_keys(
        &mut self,
        self_: Resource<HostBucket>,
        _cursor: Option<u64>,
    ) -> Result<Result<store::KeyResponse, store::Error>> {
        let name = self.table.get(&self_)?.name.clone();
        Ok(self
            .kv
            .list_keys(&name)
            .map(|keys| store::KeyResponse { keys, cursor: None })
            .map_err(kv_err))
    }

    fn drop(&mut self, rep: Resource<HostBucket>) -> Result<()> {
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
    ) -> Result<Result<u64, store::Error>> {
        let name = self.table.get(&bucket)?.name.clone();
        Ok(self.kv.increment(&name, &key, delta).map_err(kv_err))
    }
}

// ---- cache:store source + sink host impl (the cache backing store) -------

impl cache_source::Host for Host {
    fn load(&mut self, key: String) -> Result<Result<Option<Vec<u8>>, String>> {
        Ok(Ok(self.cache_backing.lock().unwrap().get(&key).cloned()))
    }
}
impl cache_sink::Host for Host {
    fn store(&mut self, key: String, value: Vec<u8>) -> Result<Result<(), String>> {
        self.cache_backing.lock().unwrap().insert(key, value);
        Ok(Ok(()))
    }
    fn remove(&mut self, key: String) -> Result<Result<(), String>> {
        self.cache_backing.lock().unwrap().remove(&key);
        Ok(Ok(()))
    }
}

// ---- wasi:config/runtime host impl ---------------------------------------

impl config::Host for Host {
    fn get(&mut self, key: String) -> Result<Result<Option<String>, config::ConfigError>> {
        Ok(Ok(self.config.get(&key).cloned()))
    }
    fn get_all(&mut self) -> Result<Result<Vec<(String, String)>, config::ConfigError>> {
        Ok(Ok(self.config.iter().map(|(k, v)| (k.clone(), v.clone())).collect()))
    }
}

// ---- config: the deployment knobs the vet-clinic components read ----------
// Sane defaults so the host runs with zero setup; override via env (VET_*).

fn build_config() -> HashMap<String, String> {
    let mut c = HashMap::new();
    let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    // auth-guard policy
    c.insert("default-tenant".into(), env("VET_TENANT", "acme-vet"));
    c.insert("session-ttl".into(), env("VET_SESSION_TTL", "3600"));
    c.insert("password-min-len".into(), "8".into());
    c.insert("audit-enabled".into(), "true".into());
    c.insert("max-attempts".into(), "5".into());
    c.insert("lockout-window".into(), "300".into());
    // secrets-vault AEAD master key (base64 of 32 bytes) — seals staff 2FA
    // secrets. Demo default; inject from a KMS in production.
    c.insert(
        "master-key".into(),
        env("VET_VAULT_KEY", "dmV0LWNsaW5pYy1kZW1vLW1hc3Rlci1rZXktMzJiISE="),
    );
    // upload-policy (pet photos) + pagination (cursor signing).
    c.insert("allowed-types".into(), env("VET_UPLOAD_TYPES", "image/png,image/jpeg,image/webp,image/gif"));
    c.insert("max-size".into(), env("VET_UPLOAD_MAX", "2097152"));
    c.insert("ticket-ttl".into(), "300".into());
    c.insert("ticket-secret".into(), env("VET_UPLOAD_SECRET", "vet-upload-secret"));
    c.insert("max-page-size".into(), env("VET_PAGE_MAX", "100"));
    c.insert("cursor-secret".into(), env("VET_CURSOR_SECRET", "vet-cursor-secret"));
    c
}

// ---- CLI -----------------------------------------------------------------

#[derive(Parser)]
#[command(name = "vet-host", about = "Run the composed vet-domain wasm over wasmtime")]
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
    /// Key-value backend: memory (default, in-process) | redis | nats. The wasm
    /// component is identical for all three — only the host store changes.
    #[arg(long, default_value = "memory")]
    kv: String,
    /// Redis URL for --kv redis.
    #[arg(long, default_value = "redis://127.0.0.1:6379")]
    redis_url: String,
    /// NATS URL for --kv nats (JetStream KV).
    #[arg(long, default_value = "127.0.0.1:4222")]
    nats_url: String,
    /// Use wasmtime's POOLING allocator (pre-reserved instance/memory slots,
    /// reused across requests) instead of the default on-demand allocator. This
    /// is what wasmCloud does — it makes per-request component instantiation of
    /// the 19-component graph far cheaper. Off by default (the naive baseline).
    #[arg(long)]
    pool: bool,
}

// ---- main: instantiate + serve -------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let mut wt_config = Config::new();
    wt_config.async_support(true);
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

    let component = Component::from_file(&engine, &args.component)?;

    // one linker: standard WASI + wasi-http + our keyvalue/config.
    let mut linker: Linker<Host> = Linker::new(&engine);
    wasmtime_wasi::add_to_linker_async(&mut linker)?;
    wasmtime_wasi_http::add_only_http_to_linker_async(&mut linker)?;
    store::add_to_linker(&mut linker, |h| h)?;
    atomics::add_to_linker(&mut linker, |h| h)?;
    config::add_to_linker(&mut linker, |h| h)?;
    cache_source::add_to_linker(&mut linker, |h| h)?;
    cache_sink::add_to_linker(&mut linker, |h| h)?;

    // pre-instantiate the proxy (incoming-handler) world once.
    let proxy_pre = ProxyPre::new(linker.instantiate_pre(&component)?)?;

    // shared, process-lifetime state.
    let kv_backend: Kv = kv::build(&args.kv, &args.redis_url, &args.nats_url)?;
    let cache_backing: CacheBacking = Arc::new(Mutex::new(HashMap::new()));
    let config = Arc::new(build_config());
    let static_dir: Arc<Option<std::path::PathBuf>> =
        Arc::new(args.static_dir.map(std::path::PathBuf::from));

    let addr: SocketAddr = args.addr.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("vet-host: serving {} on http://{}", args.component, addr);
    println!("vet-host: kv backend = {} | allocator = {}", args.kv, if args.pool { "pooling" } else { "on-demand" });
    if let Some(d) = static_dir.as_ref() {
        println!("vet-host: serving static SPA from {}", d.display());
    }

    let engine = Arc::new(engine);
    let proxy_pre = Arc::new(proxy_pre);

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let engine = engine.clone();
        let proxy_pre = proxy_pre.clone();
        let kv_backend = kv_backend.clone();
        let cache_backing = cache_backing.clone();
        let config = config.clone();
        let static_dir = static_dir.clone();

        tokio::task::spawn(async move {
            let service = hyper::service::service_fn(move |req| {
                let engine = engine.clone();
                let proxy_pre = proxy_pre.clone();
                let kv_backend = kv_backend.clone();
                let cache_backing = cache_backing.clone();
                let config = config.clone();
                let static_dir = static_dir.clone();
                async move {
                    // static SPA first (GET, non-API). Falls through to the
                    // component for API routes + all non-GET.
                    if let Some(dir) = static_dir.as_ref() {
                        if let Some(resp) = try_static(dir, &req) {
                            return Ok::<_, anyhow::Error>(resp);
                        }
                    }
                    handle_request(engine, proxy_pre, kv_backend, cache_backing, config, req).await
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

/// API route prefixes that must go to the wasm component, never to static files.
const API_PREFIXES: &[&str] = &[
    "/register", "/login", "/me", "/auth", "/pets", "/appointments", "/admin", "/i18n",
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
    let body = Full::new(bytes::Bytes::from(bytes))
        .map_err(|never| match never {})
        .boxed();
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
    proxy_pre: Arc<ProxyPre<Host>>,
    kv: Kv,
    cache_backing: CacheBacking,
    config: Arc<HashMap<String, String>>,
    req: hyper::Request<hyper::body::Incoming>,
) -> Result<hyper::Response<HyperOutgoingBody>> {
    let host = Host {
        table: ResourceTable::new(),
        wasi: WasiCtxBuilder::new().inherit_stderr().build(),
        http: WasiHttpCtx::new(),
        kv,
        cache_backing,
        config,
    };
    let mut store = Store::new(&engine, host);

    let (sender, receiver) = tokio::sync::oneshot::channel();
    // hyper::body::Incoming is already Body<Data=Bytes, Error=hyper::Error>.
    let req = store
        .data_mut()
        .new_incoming_request(wasmtime_wasi_http::bindings::http::types::Scheme::Http, req)?;
    let out = store.data_mut().new_response_outparam(sender)?;
    let proxy = proxy_pre.instantiate_async(&mut store).await?;

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
