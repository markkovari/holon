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

use bindings::wasi::config::runtime as config;
use bindings::wasi::keyvalue::atomics;
use bindings::wasi::keyvalue::store;

// ---- the in-memory key-value store ---------------------------------------
// One named bucket -> a map of key -> bytes. Shared across the whole process
// (every request handler opens buckets against the same Store), so data
// persists for the host's lifetime. A real deployment swaps this for a durable
// backend; the guest never knows.

type Buckets = Arc<Mutex<HashMap<String, HashMap<String, Vec<u8>>>>>;

/// A host resource handed to the guest when it calls `store.open(name)`.
pub struct HostBucket {
    name: String,
}

// ---- the per-request store state -----------------------------------------

struct Host {
    table: ResourceTable,
    wasi: WasiCtx,
    http: WasiHttpCtx,
    buckets: Buckets,
    config: Arc<HashMap<String, String>>,
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
        // ensure the named bucket exists.
        self.buckets
            .lock()
            .unwrap()
            .entry(identifier.clone())
            .or_default();
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
        let buckets = self.buckets.lock().unwrap();
        let val = buckets.get(&name).and_then(|b| b.get(&key)).cloned();
        Ok(Ok(val))
    }

    fn set(
        &mut self,
        self_: Resource<HostBucket>,
        key: String,
        value: Vec<u8>,
    ) -> Result<Result<(), store::Error>> {
        let name = self.table.get(&self_)?.name.clone();
        self.buckets
            .lock()
            .unwrap()
            .entry(name)
            .or_default()
            .insert(key, value);
        Ok(Ok(()))
    }

    fn delete(
        &mut self,
        self_: Resource<HostBucket>,
        key: String,
    ) -> Result<Result<(), store::Error>> {
        let name = self.table.get(&self_)?.name.clone();
        if let Some(b) = self.buckets.lock().unwrap().get_mut(&name) {
            b.remove(&key);
        }
        Ok(Ok(()))
    }

    fn exists(
        &mut self,
        self_: Resource<HostBucket>,
        key: String,
    ) -> Result<Result<bool, store::Error>> {
        let name = self.table.get(&self_)?.name.clone();
        let buckets = self.buckets.lock().unwrap();
        let exists = buckets.get(&name).map(|b| b.contains_key(&key)).unwrap_or(false);
        Ok(Ok(exists))
    }

    fn list_keys(
        &mut self,
        self_: Resource<HostBucket>,
        _cursor: Option<u64>,
    ) -> Result<Result<store::KeyResponse, store::Error>> {
        let name = self.table.get(&self_)?.name.clone();
        let buckets = self.buckets.lock().unwrap();
        let keys = buckets
            .get(&name)
            .map(|b| b.keys().cloned().collect())
            .unwrap_or_default();
        Ok(Ok(store::KeyResponse { keys, cursor: None }))
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
        let mut buckets = self.buckets.lock().unwrap();
        let b = buckets.entry(name).or_default();
        let cur: u64 = b
            .get(&key)
            .and_then(|v| std::str::from_utf8(v).ok())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let next = cur.saturating_add(delta);
        b.insert(key, next.to_string().into_bytes());
        Ok(Ok(next))
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
}

// ---- main: instantiate + serve -------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let mut wt_config = Config::new();
    wt_config.async_support(true);
    let engine = Engine::new(&wt_config)?;

    let component = Component::from_file(&engine, &args.component)?;

    // one linker: standard WASI + wasi-http + our keyvalue/config.
    let mut linker: Linker<Host> = Linker::new(&engine);
    wasmtime_wasi::add_to_linker_async(&mut linker)?;
    wasmtime_wasi_http::add_only_http_to_linker_async(&mut linker)?;
    store::add_to_linker(&mut linker, |h| h)?;
    atomics::add_to_linker(&mut linker, |h| h)?;
    config::add_to_linker(&mut linker, |h| h)?;

    // pre-instantiate the proxy (incoming-handler) world once.
    let proxy_pre = ProxyPre::new(linker.instantiate_pre(&component)?)?;

    // shared, process-lifetime state.
    let buckets: Buckets = Arc::new(Mutex::new(HashMap::new()));
    let config = Arc::new(build_config());

    let addr: SocketAddr = args.addr.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("vet-host: serving {} on http://{}", args.component, addr);

    let engine = Arc::new(engine);
    let proxy_pre = Arc::new(proxy_pre);

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let engine = engine.clone();
        let proxy_pre = proxy_pre.clone();
        let buckets = buckets.clone();
        let config = config.clone();

        tokio::task::spawn(async move {
            let service = hyper::service::service_fn(move |req| {
                let engine = engine.clone();
                let proxy_pre = proxy_pre.clone();
                let buckets = buckets.clone();
                let config = config.clone();
                async move { handle_request(engine, proxy_pre, buckets, config, req).await }
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

/// Drive one HTTP request through the component's incoming-handler.
async fn handle_request(
    engine: Arc<Engine>,
    proxy_pre: Arc<ProxyPre<Host>>,
    buckets: Buckets,
    config: Arc<HashMap<String, String>>,
    req: hyper::Request<hyper::body::Incoming>,
) -> Result<hyper::Response<HyperOutgoingBody>> {
    let host = Host {
        table: ResourceTable::new(),
        wasi: WasiCtxBuilder::new().inherit_stderr().build(),
        http: WasiHttpCtx::new(),
        buckets,
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
