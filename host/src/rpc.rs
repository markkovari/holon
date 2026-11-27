//! Cross-node invocation over wRPC (ADR-0028): binds this host's `Host` state to
//! wRPC's traits. `WrpcCtx` wants a per-invocation context, an `Invoke` client, a
//! table of shared exported resources and an optional timeout.
//!
//! Transport uses `wrpc-transport`'s framed TCP transport for the data plane, with
//! NATS core request-reply (in an instance-named queue group) for endpoint discovery
//! so callers still address targets solely by instance ID and replicas load-balance
//! and fail over without static registration.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use tokio::io::BufReader;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use wasmtime::component::{types, Component, InstancePre};
use wrpc_transport::frame::{tcp, Incoming, Outgoing, Server as FrameServer};
use wrpc_wasmtime::{ServeExt as _, SharedResourceTable, WrpcCtx};

/// Where an instance answers, and where a caller addresses it.
///
/// The instance id IS the address. Both sides derive the same string from the same
/// three names, so nothing has to be looked up or agreed at runtime — which is what
/// lets a caller invoke a component it has never seen on a node it cannot name.
pub fn prefix(lattice: &str, instance_id: &str) -> String {
    holon_lattice::wire::rpc_prefix(lattice, instance_id)
}

fn addr_subject(lattice: &str, instance_id: &str) -> String {
    format!("{}._addr", prefix(lattice, instance_id))
}

/// Framed TCP wRPC client for one target instance, discovered via NATS queue-group
/// request-reply and cached until a connection fails.
#[derive(Clone)]
pub struct RpcClient {
    nats: Arc<async_nats::Client>,
    addr_subject: String,
    cached: Arc<RwLock<Option<SocketAddr>>>,
}

impl RpcClient {
    pub fn new(nats: Arc<async_nats::Client>, lattice: &str, instance_id: &str) -> Self {
        Self {
            nats,
            addr_subject: addr_subject(lattice, instance_id),
            cached: Arc::new(RwLock::new(None)),
        }
    }

    async fn resolve(&self) -> Result<SocketAddr> {
        let reply = tokio::time::timeout(
            Duration::from_secs(5),
            self.nats.request(self.addr_subject.clone(), bytes::Bytes::new()),
        )
        .await
        .with_context(|| format!("timed out resolving wRPC endpoint `{}`", self.addr_subject))?
        .with_context(|| format!("failed resolving wRPC endpoint `{}`", self.addr_subject))?;
        let text = std::str::from_utf8(&reply.payload)
            .with_context(|| format!("non-UTF8 wRPC endpoint reply on `{}`", self.addr_subject))?;
        let addr: SocketAddr = text.trim().parse().with_context(|| {
            format!("invalid wRPC endpoint `{text}` from `{}`", self.addr_subject)
        })?;
        *self.cached.write().await = Some(addr);
        Ok(addr)
    }
}

impl wrpc_transport::Invoke for RpcClient {
    type Context = ();

    async fn invoke<P>(
        &self,
        (): Self::Context,
        instance: &str,
        func: &str,
        params: bytes::Bytes,
        paths: impl AsRef<[P]> + Send,
    ) -> Result<(Outgoing, Incoming)>
    where
        P: AsRef<[Option<usize>]> + Send + Sync,
    {
        if let Some(addr) = *self.cached.read().await {
            let owned_paths: Vec<Box<[Option<usize>]>> =
                paths.as_ref().iter().map(|p| Box::from(p.as_ref())).collect();
            if let Ok(res) = tcp::Client::from(addr)
                .invoke((), instance, func, params.clone(), owned_paths)
                .await
            {
                return Ok(res);
            }
            // Cached replica may have departed; clear and re-resolve once.
            *self.cached.write().await = None;
        }
        let addr = self.resolve().await?;
        tcp::Client::from(addr)
            .invoke((), instance, func, params, paths)
            .await
            .with_context(|| format!("wRPC TCP invoke `{instance}#{func}` at {addr}"))
    }
}

/// A client per linked interface — for EVERY link, local target or not.
///
/// There is deliberately no local short-circuit: two separately started components
/// have no in-process path between them (components that link in-process were fused
/// by `wac` at build time, ADR-0005), so skipping a co-located target leaves its
/// import unbound.
pub async fn remote_clients(
    nats: Arc<async_nats::Client>,
    lattice: &str,
    links: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, RpcClient>> {
    let mut out = BTreeMap::new();
    for (iface, target) in links {
        out.insert(iface.clone(), RpcClient::new(nats.clone(), lattice, target));
    }
    Ok(out)
}

/// What a store can invoke through, in a binary that serves two lanes.
///
/// `WrpcView` requires EVERY store to hold an `Invoke`, and a single-app run has no
/// lattice. One enum that IS an `Invoke` delegates on a lattice and refuses politely
/// off it.
pub enum Transport {
    /// On a lattice: one client per remote TARGET, keyed by the WIT interface the
    /// import came in through.
    Lattice(BTreeMap<String, RpcClient>),
    /// The single-app lane, which has no bus by design.
    Solo,
}

impl wrpc_transport::Invoke for Transport {
    type Context = ();

    async fn invoke<P>(
        &self,
        cx: Self::Context,
        instance: &str,
        func: &str,
        params: bytes::Bytes,
        paths: impl AsRef<[P]> + Send,
    ) -> Result<(Outgoing, Incoming)>
    where
        P: AsRef<[Option<usize>]> + Send + Sync,
    {
        match self {
            Transport::Lattice(by_iface) => {
                let c = by_iface.get(instance).with_context(|| {
                    format!(
                        "no remote is linked for `{instance}` — the link table named no \
                         target for it, which should have been refused at start"
                    )
                })?;
                c.invoke(cx, instance, func, params, paths).await
            }
            Transport::Solo => anyhow::bail!(
                "`{instance}#{func}` is on another node, and this host is not joined to a \
                 lattice — start it with --lattice-nats, or deploy the graph fused"
            ),
        }
    }
}

pub struct RpcCtx {
    client: Transport,
    shared: SharedResourceTable,
    timeout: Option<Duration>,
}

impl RpcCtx {
    pub fn new(client: Transport, timeout: Option<Duration>) -> Self {
        Self { client, shared: SharedResourceTable::default(), timeout }
    }
}

impl WrpcCtx<Transport> for RpcCtx {
    fn context(&self) -> <Transport as wrpc_transport::Invoke>::Context {}

    fn client(&self) -> &Transport {
        &self.client
    }

    fn shared_resources(&mut self) -> &mut SharedResourceTable {
        &mut self.shared
    }

    /// A cross-node call that never returns must not hold a guest forever.
    ///
    /// wRPC traps the component when this elapses, which is the right outcome: the
    /// request fails, the instance is discarded (it is per-request anyway), and the
    /// caller sees an error rather than a hang. Deliberately NOT retried here — see
    /// ADR-0022 on why nothing in this system is idempotent enough to retry
    /// automatically.
    fn timeout(&self) -> Option<Duration> {
        self.timeout
    }
}

fn bind_addr() -> String {
    std::env::var("HOLON_RPC_BIND").unwrap_or_else(|_| "127.0.0.1:0".to_string())
}

fn advertised_addr(local: SocketAddr) -> SocketAddr {
    if let Ok(host) = std::env::var("HOLON_RPC_ADVERTISE") {
        if let Ok(ip) = host.trim().parse::<std::net::IpAddr>() {
            return SocketAddr::new(ip, local.port());
        }
    }
    local
}

/// Actually serve this component's exports over framed TCP wRPC, advertising the
/// listener on NATS in a queue group named for the instance so replicas load-balance
/// and fail over automatically.
///
/// `make_store` is called per invocation, so a served call gets the same fresh
/// `Store` — and therefore the same scope, limits and egress policy — that an HTTP
/// request gets. A served invocation is not a privileged path.
pub async fn serve_exports_over<T>(
    engine: &wasmtime::Engine,
    component: &Component,
    pre: InstancePre<T>,
    nats: Arc<async_nats::Client>,
    lattice: &str,
    instance_id: &str,
    make_store: impl Fn() -> wasmtime::Store<T> + Send + Clone + 'static,
) -> Result<usize>
where
    T: wasmtime_wasi::WasiView + wrpc_wasmtime::WrpcView + 'static,
{
    use futures::StreamExt as _;

    let exports = exported_functions(engine, component);
    if exports.is_empty() {
        return Ok(0);
    }

    let listener = TcpListener::bind(bind_addr()).await.context("binding wRPC TCP listener")?;
    let advertise = advertised_addr(listener.local_addr().context("wRPC listener local_addr")?);

    let server = Arc::new(FrameServer::<(), BufReader<OwnedReadHalf>, OwnedWriteHalf>::new());
    let mut served = 0;
    for (iface, func, ty) in exports {
        let invocations = server
            .serve_function(
                make_store.clone(),
                pre.clone(),
                std::collections::HashMap::new(),
                ty,
                &iface,
                &func,
            )
            .await
            .with_context(|| format!("serving {iface}#{func}"))?;

        let label = format!("{iface}#{func}");
        tokio::spawn(async move {
            let mut invocations = std::pin::pin!(invocations);
            while let Some(item) = invocations.next().await {
                match item {
                    Ok(((), fut)) => {
                        let label = label.clone();
                        tokio::spawn(async move {
                            if let Err(e) = fut.await {
                                eprintln!("comp-host: serving {label} failed: {e:#}");
                            }
                        });
                    }
                    Err(e) => eprintln!("comp-host: {label} invocation stream error: {e:#}"),
                }
            }
        });
        served += 1;
    }

    // Accept loop for incoming TCP wRPC connections.
    let accept_server = server.clone();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _peer)) => {
                    let _ = stream.set_nodelay(true);
                    let (rx, tx) = stream.into_split();
                    let srv = accept_server.clone();
                    tokio::spawn(async move {
                        if let Err(e) = srv.accept((), tx, BufReader::new(rx)).await {
                            eprintln!("comp-host: wRPC accept error: {e}");
                        }
                    });
                }
                Err(e) => {
                    eprintln!("comp-host: wRPC listener accept failed: {e}");
                    break;
                }
            }
        }
    });

    // Queue-group responder so callers discover this instance's TCP address by instance ID.
    let subject = addr_subject(lattice, instance_id);
    let mut sub = nats
        .queue_subscribe(subject.clone(), instance_id.to_string())
        .await
        .with_context(|| format!("subscribing wRPC endpoint discovery on `{subject}`"))?;
    let payload = bytes::Bytes::from(advertise.to_string());
    tokio::spawn(async move {
        while let Some(msg) = sub.next().await {
            if let Some(reply) = msg.reply {
                let _ = nats.publish(reply, payload.clone()).await;
            }
        }
    });

    Ok(served)
}

/// Every function this component exports, so another node can call them.
///
/// Enumerated from the component's own type rather than from a manifest: the
/// component is the authority on what it exports, and a list maintained anywhere
/// else would drift from it silently.
///
/// Only top-level interface exports are served. A component's default (bare
/// function) exports are its own entry point — `wasi:http/incoming-handler` is the
/// obvious one — and are reached through the door rather than the bus.
pub fn exported_functions(
    engine: &wasmtime::Engine,
    component: &Component,
) -> Vec<(String, String, types::ComponentFunc)> {
    let ty = component.component_type();
    let mut out = Vec::new();
    for (name, item) in ty.exports(engine) {
        let types::ComponentItem::ComponentInstance(inst) = item.ty else {
            continue;
        };
        if name.starts_with("wasi:") {
            continue;
        }
        for (fname, fitem) in inst.exports(engine) {
            if let types::ComponentItem::ComponentFunc(f) = fitem.ty {
                out.push((name.to_string(), fname.to_string(), f));
            }
        }
    }
    out
}

/// Bind every import whose target lives on another node to a wRPC invocation.
///
/// Imports NOT in `remotes` are left alone: either the host satisfies them
/// (`wasi:*`) or they are linked locally in-process, which is ADR-0019's 1.2 ms.
pub fn link_remote_imports<V>(
    engine: &wasmtime::Engine,
    linker: &mut wasmtime::component::Linker<V>,
    component: &Component,
    remotes: &BTreeMap<String, RpcClient>,
) -> Result<usize>
where
    V: wasmtime_wasi::WasiView + wrpc_wasmtime::WrpcView + 'static,
{
    let mut bound = 0;
    for (name, ty) in linkable_imports(engine, component) {
        if !remotes.contains_key(&name) {
            continue;
        }
        let mut inst = linker
            .instance(&name)
            .map_err(|e| anyhow::anyhow!("opening linker instance `{name}`: {e}"))?;
        wrpc_wasmtime::link_instance(
            engine,
            &mut inst,
            [],
            std::collections::HashMap::new(),
            ty,
            name.clone(),
        )
        .map_err(|e| anyhow::anyhow!("linking `{name}` over wrpc: {e}"))?;
        bound += 1;
    }
    Ok(bound)
}

/// Which imports this component needs that the host cannot satisfy itself.
pub fn linkable_imports(
    engine: &wasmtime::Engine,
    component: &Component,
) -> Vec<(String, types::ComponentInstance)> {
    component
        .component_type()
        .imports(engine)
        .filter_map(|(name, item)| match item.ty {
            types::ComponentItem::ComponentInstance(inst) if !name.starts_with("wasi:") => {
                Some((name.to_string(), inst))
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fact everything else rests on: our types satisfy wRPC's context trait.
    #[test]
    fn the_host_can_satisfy_wrpcs_context_trait() {
        fn assert_ctx<T: wrpc_transport::Invoke, C: WrpcCtx<T>>() {}
        assert_ctx::<Transport, RpcCtx>();
    }

    /// Both sides derive the same address from the same three names, so a caller can
    /// invoke a component it has never seen on a node it cannot name.
    #[test]
    fn both_sides_derive_the_same_address() {
        assert_eq!(prefix("prod", "alice/shop/api"), "comp.v1.prod.rpc.alice/shop/api");
        assert!(prefix("l", "t/a/c").ends_with("t/a/c"));
    }

    /// Proves framed TCP wRPC `Server` + `tcp::Client` round-trip a call.
    #[tokio::test]
    async fn tcp_wrpc_round_trip() {
        use futures::StreamExt as _;
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        use wrpc_transport::{Invoke as _, Serve as _};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = Arc::new(FrameServer::<(), BufReader<OwnedReadHalf>, OwnedWriteHalf>::new());

        let mut invocations =
            std::pin::pin!(server.serve("demo:math/calc", "add", Arc::from([])).await.unwrap());

        let srv = server.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (rx, tx) = stream.into_split();
            srv.accept((), tx, BufReader::new(rx)).await.unwrap();
        });

        let client = tcp::Client::from(addr);
        let empty_paths: &[&[Option<usize>]] = &[];
        let call = tokio::spawn(async move {
            let (mut tx, mut rx) = client
                .invoke(
                    (),
                    "demo:math/calc",
                    "add",
                    bytes::Bytes::from_static(b"ping"),
                    empty_paths,
                )
                .await
                .unwrap();
            tx.flush().await.unwrap();
            tx.shutdown().await.unwrap();
            drop(tx);
            let mut resp = [0u8; 4];
            rx.read_exact(&mut resp).await.unwrap();
            resp
        });

        let ((), mut tx, mut rx) = invocations.next().await.unwrap().unwrap();
        let mut req = [0u8; 4];
        rx.read_exact(&mut req).await.unwrap();
        assert_eq!(&req, b"ping");
        tx.write_all(b"pong").await.unwrap();
        tx.flush().await.unwrap();
        tx.shutdown().await.unwrap();
        drop(tx);

        let resp = call.await.unwrap();
        assert_eq!(&resp, b"pong");
    }
}
