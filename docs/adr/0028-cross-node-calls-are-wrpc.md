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
