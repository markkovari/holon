//! Cross-node invocation over wRPC — the wiring, and what it still needs.
//!
//! ADR-0028 established that this is wRPC's job rather than a codec of our own.
//! This module is the integration point, and it exists now to settle the question
//! everything else depends on: **can this host's `Host` state satisfy wRPC's
//! traits?** If it could not, the whole approach would need rethinking, and finding
//! that out after building placement and a two-machine harness would be expensive.
//!
//! It can. `WrpcCtx` wants four things — a per-invocation context, an `Invoke`
//! client, a table of shared exported resources, and an optional timeout — and all
//! four are things a `Scope` already knows or a NATS client already is.
//!
//! **This is not wired.** Nothing calls it yet: no import is bound to a remote
//! instance and no export is served. What remains is listed at the bottom, and it
//! is placement and lifecycle work rather than protocol work.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use wasmtime::component::{types, Component, InstancePre};
use wrpc_runtime_wasmtime::{ServeExt as _, SharedResourceTable, WrpcCtx};
use wrpc_transport_nats::Client as NatsInvoke;

/// Where an instance answers, and where a caller addresses it.
///
/// The instance id IS the address. Both sides derive the same string from the same
/// three names, so nothing has to be looked up or agreed at runtime — which is what
/// lets a caller invoke a component it has never seen on a node it cannot name.
pub fn prefix(lattice: &str, instance_id: &str) -> String {
    format!("comp.{lattice}.rpc.{instance_id}")
}

/// A client addressed at one instance.
///
/// `queue_group` is `Some` on the SERVE side and `None` on the call side: replicas
/// of one component subscribe to the same group so NATS hands each invocation to
/// exactly one of them, which is where failover and load spreading come from for
/// free. A caller joins no group — it is asking, not answering.
pub async fn client(
    nats: Arc<async_nats::Client>,
    lattice: &str,
    instance_id: &str,
    queue_group: Option<&str>,
) -> Result<NatsInvoke> {
    NatsInvoke::new(nats, prefix(lattice, instance_id), queue_group.map(Arc::from))
        .await
        .with_context(|| format!("wrpc client for {instance_id}"))
}

/// Per-instance wRPC state, hung off `Host` beside the `Scope`.
///
/// One connection per node: an invocation carries its own subject, so nothing about
/// the client is tenant-specific. What IS tenant-specific — which subject an import
/// resolves to — lives in the link table on the `Scope`, and never in here.
pub struct RpcCtx {
    /// `Invoke` is implemented on `Client` itself, not on `Arc<Client>`, so the
    /// client is cloned per instance rather than shared behind a pointer. It is a
    /// handle over one NATS connection, so a clone is cheap — the connection is
    /// not duplicated.
    client: NatsInvoke,
    shared: SharedResourceTable,
    timeout: Option<Duration>,
}

impl RpcCtx {
    pub fn new(client: NatsInvoke, timeout: Option<Duration>) -> Self {
        Self { client, shared: SharedResourceTable::default(), timeout }
    }
}

impl WrpcCtx<NatsInvoke> for RpcCtx {
    /// Per-invocation headers. Nothing needs them yet; when tracing crosses a node
    /// boundary this is where the span context goes.
    fn context(&self) -> <NatsInvoke as wrpc_transport::Invoke>::Context {
        None
    }

    fn client(&self) -> &NatsInvoke {
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

/// Serve every function this component exports, so another node can call them.
///
/// Enumerated from the component's own type rather than from a manifest: the
/// component is the authority on what it exports, and a list maintained anywhere
/// else would drift from it silently.
///
/// Only top-level interface exports are served. A component's default (bare
/// function) exports are its own entry point — `wasi:http/incoming-handler` is the
/// obvious one — and are reached through the door rather than the bus.
pub fn serve_exports<T>(
    engine: &wasmtime::Engine,
    component: &Component,
    pre: InstancePre<T>,
    client: &NatsInvoke,
    store: impl Fn() -> wasmtime::Store<T> + Send + Clone + 'static,
) -> Vec<(String, String, types::ComponentFunc)>
where
    T: wasmtime_wasi::WasiView + wrpc_runtime_wasmtime::WrpcView + 'static,
{
    let _ = (pre, client, store);
    let ty = component.component_type();
    let mut out = Vec::new();
    for (name, item) in ty.exports(engine) {
        let types::ComponentItem::ComponentInstance(inst) = item else {
            // A bare function export is the component's own entry point, not
            // something another component links against.
            continue;
        };
        // wasi:* exports are the host's business (incoming-handler), never a peer's.
        if name.starts_with("wasi:") {
            continue;
        }
        for (fname, fitem) in inst.exports(engine) {
            if let types::ComponentItem::ComponentFunc(f) = fitem {
                out.push((name.to_string(), fname.to_string(), f));
            }
        }
    }
    out
}

/// Which imports this component needs that the host cannot satisfy itself.
///
/// These are the candidates for a link — locally to another instance in this
/// process, or over wRPC to one on another node. Anything the host provides
/// (`wasi:*`) is excluded here rather than at the call site, so a new host
/// capability does not accidentally become a remote call.
pub fn linkable_imports(
    engine: &wasmtime::Engine,
    component: &Component,
) -> Vec<(String, types::ComponentInstance)> {
    component
        .component_type()
        .imports(engine)
        .filter_map(|(name, item)| match item {
            types::ComponentItem::ComponentInstance(inst) if !name.starts_with("wasi:") => {
                Some((name.to_string(), inst))
            }
            _ => None,
        })
        .collect()
}

// ---- the blocker, stated precisely ----------------------------------------
//
// Two problems remain, and the second is a design question rather than work.
//
// 1. `impl WrpcView for Host` requires every store to hold a live `Invoke` client.
//    A single-app run (`comp-host --component x.wasm`) has NO NATS — that lane's
//    entire point is that it needs no broker, and it is the lane 30-odd example
//    recipes and the whole self-hosting story use. So the trait cannot be
//    satisfied honestly in one of the two lanes this binary serves.
//
//    Three ways out, none obviously right:
//      a. Make `Host` generic over the transport, with a null implementation for
//         the single-app lane. Correct, and it touches every capability impl and
//         every `Store` construction on the request path.
//      b. Split the two lanes into different `Host` types. Duplicates the
//         capability impls, which are the security-critical ones (ADR-0023) and
//         are exactly what should not exist twice.
//      c. Connect a NATS client unconditionally. Cheapest, and it silently makes
//         the self-hosting lane depend on a broker, which is a lie about what the
//         lane is for.
//
// 2. `link_instance` resolves its target from the client held in `WrpcCtx`, so one
//    store implies ONE remote prefix. A component importing from two different
//    remote instances therefore cannot be served by one client, and the link table
//    is explicitly a map of MANY ifaces to MANY instance ids. Whether wRPC expects
//    a client per target, or a prefix that is a namespace rather than an address,
//    needs reading its invocation path properly — not guessing from a signature.
//
// What IS settled and worth keeping:
//   * `WrpcCtx` is satisfiable by our types (the test below).
//   * The addressing scheme: `comp.<lattice>.rpc.<tenant>/<app>/<component>`, with
//     a queue group on the serve side so replicas share invocations and failover is
//     free — the thing NATS queue groups were wanted for from the start.
//   * Which exports are worth serving and which imports are candidates for linking,
//     both read from the component's own type rather than from a manifest that
//     could drift from it.
//   * async-nats is unified at 0.49 across host, lattice and reconciler. That was a
//     hard blocker — wrpc-transport-nats needs 0.49 and everything else was on
//     0.38, giving two incompatible `async_nats::Client` types — and it is fixed.

#[cfg(test)]
mod tests {
    use super::*;

    /// The fact everything else rests on: our types satisfy wRPC's context trait.
    /// Compile-time, because there is no behaviour yet to assert and constructing a
    /// live client to check a type would be a worse test rather than a better one.
    #[test]
    fn the_host_can_satisfy_wrpcs_context_trait() {
        fn assert_ctx<T: wrpc_transport::Invoke, C: WrpcCtx<T>>() {}
        assert_ctx::<NatsInvoke, RpcCtx>();
    }

    /// Both sides derive the address from the same three names, so a caller can
    /// invoke a component it has never seen on a node it cannot name. If these ever
    /// disagree, every cross-node call silently finds no responder.
    #[test]
    fn both_sides_derive_the_same_address() {
        assert_eq!(prefix("prod", "alice/shop/api"), "comp.prod.rpc.alice/shop/api");
        // The instance id is the address, so it must survive verbatim — a sanitiser
        // here would make the caller and the server disagree.
        assert!(prefix("l", "t/a/c").ends_with("t/a/c"));
    }
}
