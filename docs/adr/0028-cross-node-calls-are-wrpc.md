# ADR-0028 — Cross-node calls are wRPC, and the codec I designed should never have existed

- **Status:** accepted
- **Date:** 2026-08-08
- **Corrects:** [ADR-0025](0025-slice-one-on-the-lattice.md)'s reasoning for deferring cross-node calls

## What was wrong

ADR-0025 deferred cross-node WIT calls, and gave two reasons. Both were wrong.

**"A `Val` ↔ JSON codec, driven by the import's reflected type … any WIT function becomes
callable over NATS with no codegen and no `wrpc`."** Treating a dependency as the thing to
avoid, and hand-writing a serialization layer for the component model as the cheap
alternative. It is not the cheap alternative. wRPC already specifies one, and uses the
[component model value definition encoding] on the wire — the canonical binary form. JSON
cannot represent the type set faithfully anyway: `u64` loses precision, `char` and binary
need conventions, floats are lossy.

The stated justification — "no codegen" — was also based on a wrong belief. wRPC
**explicitly supports dynamic invocation**, "based on e.g. runtime WebAssembly component
type introspection". That is exactly what binding an unsatisfied import from a reflected
surface at start time is.

**"A hard ceiling: resource handles and streams cannot cross a node."** Asserted without
checking. wRPC *fully supports* `stream` and `future` — it defines a framing format that
multiplexes an invocation's asynchronous data streams, each identified by an index, over
one bidirectional byte stream. And resources are "encoded as opaque byte blobs, `list<u8>`,
and their meaning is entirely application specific": they *can* cross, they are simply not
automatically meaningful, which is a much narrower statement than the one in ADR-0025.

## Decision

**Cross-node invocation is wRPC over NATS.** No invented codec.

wRPC is a bytecodealliance project, not a wasmCloud-internal one, so adopting it does not
re-adopt what this platform left. ADR-0012 was about the **provider and host** model naming
buckets from guest strings. The RPC layer was never implicated, and the lattice shape here
was always wasmCloud's — this is the one well-specified piece that got skipped.

## The version pin, which is the real cost

`wrpc-runtime-wasmtime` 0.31 builds against **wasmtime 45**; this host was on **47**.
Taking both puts two incompatible `Store`/`Linker`/`Component` types in one dependency
tree — the same shape of problem as the old `nats`/`rand` conflict, and equally not
negotiable.

So `host/` is pinned to wasmtime 45. Measured before committing to it:

* every wasmtime API this host uses is **identical** across 45 and 47 — the downgrade
  produced zero compile errors;
* the full suite passed unchanged (25 host tests);
* the adversarial run still says **contained** — 0 foreign opens, 0 keys, 0 lateral
  connections — at 10,075 rps, p50 4.80 ms, p99 10.01 ms, which is within noise of the
  10,477 / 4.64 / 9.26 measured on 47;
* two replicas still share one store across nodes.

The alternative was to take only `wrpc-transport` (which is runtime-independent — verified,
it pulls no wasmtime at all) and write the wasmtime-47 glue by hand. That keeps a newer
runtime at the cost of maintaining the integration layer forever, to stay one minor
version ahead of a dependency that will move anyway. Following wRPC and bumping when it
bumps is the cheaper direction, and the pin is documented in `host/Cargo.toml` with
instructions to move all four crates together.

## Integration risk, settled

The question everything else rests on is whether this host's `Host` state can satisfy
wRPC's traits. It can, and `host/src/rpc.rs` proves it at compile time rather than leaving
it to be discovered after placement and a two-machine harness were built on the assumption.

`WrpcCtx` wants four things — a per-invocation context, an `Invoke` client, a table of
shared exported resources, and an optional timeout — and all four are things a `Scope`
already knows or a NATS client already is. One wrinkle found and recorded: `Invoke` is
implemented on `Client`, not on `Arc<Client>`, so the client is cloned per instance rather
than shared behind a pointer. It is a handle over one connection, so that is cheap.

## Two blockers, found by trying

Attempting the wiring surfaced one hard dependency problem, now fixed, and one design
question that is not mine to settle by guessing.

**Fixed: async-nats was split.** `wrpc-transport-nats` 0.31 needs async-nats **0.49**; the
host, the lattice crate and the reconciler were all on **0.38**, giving two incompatible
`async_nats::Client` types in one tree. The same shape as the wasmtime pin and equally
non-negotiable. All three are on 0.49 and everything still passes.

**Resolved, and my analysis of it was wrong.** Both "blockers" below dissolved on reading
`Invoke` itself instead of inferring from `link_instance`'s signature.

*The multi-target problem did not exist.* `Invoke::invoke(&self, cx, instance, func, …)`
takes the instance as a **per-call argument**. A store is not limited to one remote target;
the transport can dispatch on the interface name, which is exactly what a link table needs.

*The two-lane problem did not need a refactor.* `Invoke` is a plain trait with three
associated types, so a small enum can BE one: `Transport::Lattice(map)` delegates,
`Transport::Solo` refuses with a sentence naming the fix. Borrowing the associated types
from the NATS client means the refusing variant never constructs a stream it cannot make.
About thirty lines, against the "make `Host` generic over the transport" refactor I had
recommended. Recorded because the wrong recommendation was confident and would have cost
days.

**Was open: `WrpcView` demands a client that one lane does not have.** The trait requires every
`Store` to hold a live `Invoke` client. A single-app run — `comp-host --component x.wasm` —
has no NATS at all, and that is the *point* of that lane: it is what the self-hosting story
and around thirty example recipes use. Three ways out, none obviously right:

| | cost |
|---|---|
| make `Host` generic over the transport, null impl for the single-app lane | correct; touches every capability impl and every `Store` on the request path |
| split the lanes into two `Host` types | duplicates the capability impls — the security-critical ones (ADR-0023), exactly what must not exist twice |
| connect NATS unconditionally | cheapest; silently makes the broker-free lane depend on a broker, which is a lie about what the lane is for |

**Also open: one store implies one remote prefix.** `link_instance` resolves its target from
the client in `WrpcCtx`, so a component importing from two different remote instances cannot
be served by one client — and a link table is explicitly many interfaces to many instance
ids. Whether wRPC wants a client per target, or treats the prefix as a namespace rather than
an address, needs reading its invocation path properly rather than inferring from a
signature.

*(Historical: this was the reasoning at the time. The enum above is the answer none of the
three options was.)* I stopped rather than pick (c) because it is cheap. That would trade a lie in the
architecture for a green tick, and the lane it damages is the one people actually run.

## The call side, wired

`Host` implements `WrpcView`. At start, an instance's link table is split: targets running
in this process keep the direct in-process path, and the rest become wRPC clients keyed by
the interface they arrive through. `link_remote_imports` then binds those interfaces in the
linker **before** `instantiate_pre`, so an import with neither a host impl nor a link still
fails at start — omission stays fail-closed (ADR-0013).

A bound import is indistinguishable from a local one to the guest: it calls a function, and
whether that crosses a machine is a placement decision it never sees. That is what the
component model is for, and why this belongs in the linker rather than at a call site.

Resources are explicitly not carried across yet — `link_instance` is given empty resource
maps. wRPC encodes a resource as opaque bytes whose meaning is application-specific, and
nothing here has defined that meaning, so an interface carrying one must be refused at
placement rather than handed a blob the far side cannot interpret.

## Still missing: the serve side

Nothing yet exposes a component's exports over wRPC, so a bound import has nobody to call.
`ServeExt::serve_function` is the mirror of the above and `serve_exports` already enumerates
what to serve; what remains is spawning those tasks at start with the instance's own subject
prefix and a queue group, and letting `plan.rs` place a graph across nodes instead of
co-locating it. Until both exist there is no end-to-end test, and this is **not** claimed to
work.

## What is settled and kept

* `WrpcCtx` is satisfiable by our types — asserted at compile time.
* **The addressing scheme**: `comp.<lattice>.rpc.<tenant>/<app>/<component>`, with a queue
  group on the serve side so replicas share invocations and failover is free. That is what
  NATS queue groups were wanted for from the start.
* **Which exports are worth serving and which imports are linkable**, both read from the
  component's own type rather than from a manifest that could drift from it.

## What is still not built

The deferral itself stands: nothing in this repo has yet made a WIT call over a wire. The
remaining work is placement and lifecycle rather than protocol, in this order:

1. `Host` gains an `RpcCtx` and an `impl WrpcView` — one method returning
   `WrpcCtxView { ctx, table }`. Cheap, but it changes `Host` construction on the
   per-request path, so it wants measuring after.
2. **Call side**: at start, for every link-table entry whose target is not in the local
   instance table, `polyfill::link_function` binds that import to a wRPC invocation. The
   local case must keep the direct in-process path — that is ADR-0019's 1.2 ms and the
   entire reason for co-locating by default.
3. **Serve side**: `ServeExt::serve_function` for each export another node might call, on
   the instance's own subject with a queue group so replicas share the work.
4. **Placement**: `plan.rs` co-locates every component of an app onto the root's nodes
   today. Spanning means placing them independently and marking each link-table entry
   local or remote — which is also where a graph whose edges cannot be remoted must be
   refused rather than discovered on first call.
5. **Which interfaces are remotable**, below, which (4) cannot enforce until something
   classifies them.
Graphs co-locate, and a `linked` plug still needs a fused artifact. What changed is that
the work ahead is *wiring an existing protocol* rather than *designing a wire format*, and
the pieces are in the tree ready for it.

Two things remain genuinely open, and neither is about encoding:

* **Which interfaces are safely remote.** Resources crossing as opaque bytes means the
  application must define what those bytes mean. An interface passing a resource whose
  meaning is a pointer into one process is still not remotable, and something has to refuse
  that at placement time rather than at first call.
* **Routing across replicas.** Two nodes serving one app does nothing for a caller that
  only knows one address. That is a separate, smaller problem than invocation, and it is
  the one that makes multi-node placement useful rather than merely true.

[component model value definition encoding]: https://github.com/WebAssembly/component-model/blob/main/design/mvp/Binary.md
