# ADR-0103 — components move to WASI p3, on the rc WIT until wrpc moves

*An HTTP component should be one live instance serving its requests, not a
fresh instantiation per request.*

**Status: accepted, in progress.** comp-host serves p3 and p2 components side
by side; HTTP components are being ported.

## The problem

Every p2 request instantiates the guest (ADR-0037). That is what makes an idle
app free, and it is also most of what a request costs: in HOST-PERF Round 12
the p2 `bench-suite` spends 87 µs of host CPU on a request that does nothing.
A p2 guest also cannot stream across a component boundary, which is why
ADR-0102's model call is native for now.

## The decision

- **comp-host serves both generations from one linker.** The door is chosen
  from the component's exports: `wasi:http/incoming-handler@0.2` is p2,
  `wasi:http/handler@0.3` is p3. Nothing is configured.
- **A p3 request is a task on a live instance.** wasmtime-wasi-http's
  `ProxyHandler` queues requests onto a worker: up to 128 requests per
  instance, 16 at a time, dropped after 1 s idle (`wasmtime serve`'s p3
  defaults). Stores come from the same `store_for` as p2, so tenant boundary,
  memory cap, CPU slice and egress policy are the same code.
- **The WIT is `0.3.0-rc-2026-03-15`, not final `0.3.0`.** comp-host is held
  on wasmtime 45 by `wrpc-runtime-wasmtime` 0.31, and 45's opt-in `p3`
  implements the rc; final `0.3.0` arrives in 46 and is on by default from 49.
  `wit/p3` vendors the rc set as one `generate!` path.
- **Only HTTP components move.** Plugs that export sync interfaces stay p2: a
  p3 socket composes with p2 plugs (`wac-graph` 0.10 handles the mix, and one
  linker serves both). `wasi:keyvalue`, `wasi:config` and `wasi:blobstore`
  have no p3 version and stay p2 imports inside p3 components.
- **p3 egress is the p2 egress.** The allow-list and resolved-address check
  and the buffered reqwest fetch are shared functions. Upstream's default p3
  `send_request` is unrestricted, so the hook is always overridden.

## Consequences

- **ADR-0037's "an instance is per request" holds for p2 only.** A p3
  instance spans up to 128 requests of one tenant: its memory cap is shared by
  those requests, and a secret revealed by one is cached for the others. An
  app with p3 traffic is not free until 1 s after its last request.
- **Throughput.** Round 12: p3 with reuse serves 1.5× p2's requests at about
  half the CPU per request. One p3 instance *per request* is slower than p2
  (101 vs 87 µs), so reuse is not optional.
- **Toolchain.** A p3 crate depends on `wit-bindgen` 0.58 (async `generate!`),
  not cargo-component's `wit-bindgen-rt`. cargo-component claims any crate
  with a `wit/` and cannot resolve a p3 world, so its bindings step is
  `cargo xtask bindings`, which names only the p2 crates. `guestio` and
  `guestauth` have p3 variants of their macros.
- **Module names.** wit-bindgen suffixes a module with its version when two
  versions of a package share a resolve, and the root `auth.wit` puts p2
  `wasi:http` beside p3's in most components. Code names `bindings::p3::{http,
  handler, clocks, random}`, aliases each crate defines, so only the aliases
  move when the WIT version does.

## The exit

When wrpc supports wasmtime ≥ 46: bump comp-host, point `wit/p3`'s links at
final `0.3.0`, move each p3 world's `@0.3.0-rc-2026-03-15` to `@0.3.0`, and
fix the suffix in each `bindings::p3` alias. Handler code does not change.
