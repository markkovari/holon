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

use std::time::Duration;

use wrpc_runtime_wasmtime::{SharedResourceTable, WrpcCtx};
use wrpc_transport_nats::Client as NatsInvoke;

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

// ---- what is still missing ------------------------------------------------
//
// The protocol question is settled; these are the remaining pieces, in the order
// they have to happen:
//
// 1. `Host` gains an `RpcCtx` and an `impl WrpcView`, which is one method
//    returning `WrpcCtxView { ctx, table }`. Cheap, but it changes `Host`'s
//    construction on the per-request path, so it wants measuring after.
//
// 2. START, call side: for every link-table entry whose target is NOT in the local
//    instance table, `polyfill::link_function` binds that import to a wRPC
//    invocation instead of leaving it unsatisfied. The local case must keep the
//    direct in-process path — that is ADR-0019's 1.2 ms and the whole reason for
//    co-locating by default.
//
// 3. START, serve side: `ServeExt::serve_function` over the node's NATS client for
//    each export another node might call, with the instance's own subject and a
//    queue group so replicas share the work.
//
// 4. PLACEMENT: `plan.rs` co-locates every component of an app onto the root's
//    nodes today (see `holds_state` and the `Linked` branch). Spanning means
//    placing components independently and marking each link-table entry local or
//    remote — which is also where a graph whose edges cannot be remoted has to be
//    refused, rather than discovering it on the first call.
//
// 5. WHICH INTERFACES ARE REMOTABLE. wRPC encodes resources as opaque `list<u8>`
//    whose meaning is application-specific, so an interface passing a resource
//    that is really a pointer into one process is still not remotable. Nothing
//    audits the catalogue for this, and (4) cannot refuse what nobody classified.

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of this module today: prove the trait bound holds against
    /// the real NATS client type, at compile time, BEFORE placement and a
    /// two-machine harness get built on the assumption that it does.
    ///
    /// A compile-time assertion with an empty body is exactly as much as is
    /// warranted — it asserts the shape, and there is no behaviour yet to assert.
    /// Constructing a live client to check a type would be a worse test, not a
    /// better one.
    #[test]
    fn the_host_can_satisfy_wrpcs_context_trait() {
        fn assert_ctx<T: wrpc_transport::Invoke, C: WrpcCtx<T>>() {}
        assert_ctx::<NatsInvoke, RpcCtx>();
    }
}
