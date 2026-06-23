# Host performance — Node/jco vs native Rust (wasmtime)

The same vet-clinic app runs on three hosts (see `examples/jco-vet-domain`,
`host/`, `examples/vet-clinic-wasmcloud`). This compares the **HTTP request**
throughput / latency / memory of the hosts running it, to put numbers on the
"which host" choice. The capability components are identical Rust `.wasm` in
every case — what differs is the host and (for jco) the domain language.

> **Read this as stack-vs-stack, not language-vs-language.** The jco row runs
> the TypeScript `domain.ts` under Node+Fastify; the Rust rows run the Rust
> `vet-domain` component under a native wasmtime host. Different domain code AND
> different runtime. It is **not** a controlled apples-to-apples microbenchmark —
> it's "what does each deployable host actually deliver for this app."

## Method

- One machine (Apple Silicon, macOS), all hosts local, no k8s.
- Load: [`oha`](https://github.com/hatoo/oha), 15 s (reads) / 10 s (writes),
  50 conns (reads) / 20 conns (writes).
- Three operations:
  - **GET /pets** — hot read (auth introspect + a `records:store` indexed
    lookup). The cheapest path → best isolates per-request host overhead.
  - **POST /auth/login** — argon2 password verify (dominates; same wasm hash
    cost on every host) + session issue.
  - **POST /pets** — validate + `records:store` write + `search:index`.
- RSS sampled (`ps`) on the listening PID under sustained read load.
- Same seeded data; in-memory KV on all hosts for the latency runs.

## Results (representative, single machine)

| host / mode | GET /pets (req/s) | login (req/s) | POST /pets (req/s) | RSS under load | artifact |
|---|--:|--:|--:|--:|--|
| **Node + jco** (TS domain) | **3876** | 38 | 278 | 48 MB | 273 MB `node_modules` |
| **Rust host — on-demand alloc** | 503 | 165 | 514 | **24 MB** | 18.8 MB bin + 2.6 MB wasm |
| **Rust host — pooling alloc** (`--pool`) | 2957 | 175 | 1793 | 427 MB | 18.8 MB bin + 2.6 MB wasm |
| **wasmCloud (k8s, full app, LINKED)** | ~6 (GET /) / ~2.6 (/pets) | — | — | per-host reservation | 21 OCI images |

(The wasmCloud row is the **full 21-component app** as a linked lattice — it
deploys + serves UI + every feature. The low rps is **per-request component
instantiation on the on-demand allocator** (~95 ms floor even for a static
response), NOT a wasmCloud ceiling — see "Why only ~6–10 rps?" below. The local
**pooling** row is what this becomes with the allocator a production wasmCloud
host uses.)

(Latency tracks throughput inversely: e.g. GET /pets mean was ~13 ms Node,
~100 ms Rust on-demand, ~17 ms Rust pooled.)

## What the numbers say

### The read-path gap is allocator, not language
The native Rust host serves each request in a **fresh wasmtime `Store`** (for
isolation) and instantiates the **19-component composed graph** per request.
With the **default on-demand allocator** that's fresh mmaps + table setup every
time → on a trivial read, *instantiation IS the cost* (503 req/s, 8× slower than
Node, which loads the wasm once and keeps it warm).

Switching to wasmtime's **pooling allocator** (`--pool`: pre-reserved,
recycled instance/memory/table slots — **the strategy wasmCloud uses**) collapses
that: **503 → 2957 req/s, ~6× faster**, into Node's ballpark. So the gap was the
host's allocation strategy, not Rust or wasmtime being slow.

### On work-heavy paths Rust already wins
Where the request does real work, per-request instantiation is amortized and the
native host is faster regardless of allocator:
- **login (argon2-bound): 165–175 vs 38 req/s — ~4.4× faster.** Node's argon2 is
  the bottleneck; the Rust component's is tighter.
- **POST /pets (write+index): 514 → 1793 vs 278 — up to 6.5× faster.**

### Memory is a real trade
- **On-demand Rust: 24 MB** under load vs **Node 48 MB** — ~2× lighter, and the
  artifact is a single 18.8 MB static binary + a 2.6 MB wasm vs **273 MB of
  `node_modules`**.
- **Pooling Rust: 427 MB** — the pooling allocator pre-reserves all its slots
  up front (here generous caps: ~10 k memory slots × up to 64 MiB). That's the
  trade: pooling buys instantiation speed with reserved memory. **Tunable** —
  shrink the caps in `host/src/main.rs` (`PoolingAllocationConfig`) to fit the
  real concurrency and the footprint drops accordingly.

## Estimated resource picture per deployment

| | Node + jco | Rust on-demand | Rust pooling |
|---|---|---|---|
| cold start | ~node boot + jco load | instant (single binary) | instant + slot pre-reserve |
| steady RSS | ~48 MB | ~24 MB | ~100–430 MB (cap-dependent) |
| read latency | low (warm module) | high (per-req instantiate) | low |
| write/auth latency | high (JS argon2) | low | low |
| image / footprint | 273 MB deps | ~21 MB total | ~21 MB total |
| best for | read-heavy, dev ergonomics | memory-tight, write/auth-heavy | balanced prod (the wasmCloud model) |

**Rule of thumb:** for a production deploy of a composed wasm app, use the
**pooling allocator** (or wasmCloud, which does this for you) and size the pool
to expected concurrency — you get Node-class read throughput, multiples better
write/auth throughput, a ~21 MB artifact, and a memory footprint you dial in.

## wasmCloud (k8s) — what actually happened

Deployed against the live in-cluster `wasmcloud-operator` host (v1.6.0,
JetStream NATS), `examples/vet-clinic-wasmcloud/k8s`. Two concrete findings:

### 1. The full 19-component app does NOT deploy on the wasmCloud host
```
failed to compile component:
  The component transitively contains 104 core module instances,
  which exceeds the configured maximum of 30
```

> **Version note.** "wasmCloud 2.x" elsewhere in this repo is shorthand for the
> **Kubernetes-operator deployment model** (CRD-driven), NOT a 2.0 host. The
> host BINARY is 1.x — `1.6.0` for this deploy, `1.4.1` on the standing
> `comp-auth` host; the operator is `0.4.0` (`k8s.wasmcloud.dev/v1alpha1`).
> There is no 2.0 host. The cap below is the same on 1.4.1 and 1.6.0.

**Root cause — wrong topology, NOT a density limit.** This is the important
correction: the "30" is **not** how many components a host can run. wasmCloud
runs **1000s** of component instances per host — that's its whole pitch,
governed by the pooling allocator's `total_component_instances` /
`--max-components` (default 10000). That limit was nowhere near hit.

The "30" is a **different** wasmtime knob: the max number of core-module
instances **nested inside ONE component's graph** (`InstanceLimits`, default 30).
The error says it exactly — the *single* `vet_domain.full.composed.wasm`
"transitively contains 104 core module instances". The failure is that I
`wac plug`'d **all 19 capabilities into one fused mega-component**, and that
single artifact's internal graph is too deep — not that the host can't hold many
components.

Why one artifact is 104: each component, built independently by
`cargo-component`, bundles its own WASI preview1 adapter + bindgen glue (~4 core
modules); 19 fused = ~100. (`wac` doesn't dedupe the adapter.)

| artifact | components | core modules |
|---|--:|--:|
| one component (e.g. record-store) | 1 | 4 |
| auth-guard.composed | 3 | 12 |
| vet_domain.full.composed | 19 | ~100 |

**The fix is the idiomatic wasmCloud topology, and it's how the auth app already
works.** The deployed auth app does NOT fuse its pieces — it runs `accounts-app`
and `auth-guard` as **two separate components, linked by wadm** (`accounts-app`
→ `auth:identity` → `auth-guard`). The host runs them as independent instances
and wires them at the lattice. Applied to vet-domain: deploy **vet-domain as one
component LINKED to ~18 separate capability components** (each small, ~4
instances, far under 30), instead of one fused blob. Then nothing exceeds the
per-component limit, and the host happily runs all 19 + their links — density is
a non-issue.

Fusing into one `.wasm` was a convenience for the **jco / native-host** path
(one artifact, host satisfies WASI). That convenience is exactly what breaks the
wasmCloud deploy — and ironically defeats wasmCloud's strength (per-component
scaling, linking, hot-swap, density). **Same components, two deployment shapes:**
fuse for a single-process host (jco/native), link for wasmCloud.

So the full app on wasmCloud is a **manifest exercise** (linked components +
their wadm links), not blocked by Rust, wasm, or any host limit.

**Built + verified.** `examples/vet-clinic-wasmcloud/gen-manifest.py` generates
the linked topology — **21 components** (vet-domain + 18 capabilities +
http-server/http-client/keyvalue-nats providers), each pushed separately, wired
by wadm links. It **deploys clean and the full app runs on wasmCloud k8s**:

```
seed RBAC 204 · register 201 · login token · pet (records:store→NATS KV) ULID
fsm confirm 200 · invoice 45.00 (money) · note 201 (md:render + lock:mutex)
i18n/es 200 (i18n:catalog) · AI summary 200 (ai:inference + cache)
```

GET /pets throughput: **~2.6 req/s**. That's the genuinely-distributed cost: a
single request now fans out across **multiple wrpc hops over NATS** (vet-domain
→ authorizer → auth-guard → keyvalue-nats; → records-store → keyvalue-nats; →
search-index → keyvalue-nats), each a network round-trip, on one host replica
through a port-forward. The trade vs the fused single-process hosts is explicit:
you buy **independent per-component scaling / linking / hot-swap / density**
(the lattice) and pay **inter-component + provider latency** for it. Tune by
scaling `spreadscaler` replicas, co-locating hot links, and running load
in-cluster.

Two bugs found + fixed getting there: (1) the fused-blob instance-cap above;
(2) a missing `i18n:catalog` wadm link → runtime wrpc trap on first invoke
(wadm doesn't validate that every guest import has a link; it fails at call
time). Both are manifest/composition issues, not runtime limits.

### Serving EVERYTHING from the lattice — UI + API, no Node

The React SPA is **embedded into the vet-domain wasm** (`build.rs` →
`include_bytes!` over `static/`, ~620 KB; the composed app is ~3.4 MB). The
component serves its own UI: `GET /` → index.html, `GET /assets/*` → the bundle,
everything else → API or SPA-fallback. So the http-server provider routes UI AND
API to the one component — **no static-file provider, no Node, nothing outside
the lattice.** Verified on the live k8s deploy: `GET /` → 200 html, `/assets/
*.js` → 200, login/pet/fsm/invoice/i18n/ai all green.

Exposed via a **NodePort Service** (`k8s/vet-domain-service.yaml`, `:30081` →
host `:8081`) — reachable at `http://localhost:30081` with **no port-forward**
(single requests verified). In-cluster throughput (oha pod → ClusterIP, the
honest path without the orbstack localhost-NodePort quirk that refuses rapid
new conns): **GET / ≈ 10.5 rps** (UI, static-from-wasm — even a static response
pays the http-server↔component wrpc-over-NATS hop), **GET /pets ≈ 2.6 rps**
(full lattice fan-out). Both reflect the distributed-lattice cost, not the
runtime; scale `spreadscaler` replicas + co-locate hot links to improve.

**Bottom line:** the entire vet-clinic — React UI + 19-capability domain — runs
as pure wasm on wasmCloud in k8s. Same Rust components also run, fused, on jco
and the native wasmtime host. One set of components, three hosts; only the
*shape* (fuse for single-process, link for the lattice) and the *exposure*
differ.

### Why only ~6–10 rps? — latency breakdown (NOT a wasmCloud ceiling)

10 rps for a wasm app looks absurd. It is — and it's a stack of fixable,
environment-specific costs, not wasmCloud's ceiling. Measured layer by layer:

| measurement | value | what it tells us |
|---|--:|---|
| raw NATS RTT, in-cluster | **~1 ms** | transport is fast — not the bottleneck |
| `GET /` (static UI: no KV, no auth, no seed) | **~96 ms** | the floor is the *invocation itself* |
| 96 ms ÷ 1 ms | ~95× | ~95 ms is spent NOT in transport |

The ~95 ms floor on a *static* response (just return embedded bytes) means the
cost is **per-request component instantiation on the wasmCloud host**. Every
HTTP request, the host builds a fresh wasmtime `Store` and instantiates the
composed component — here ~100 core-module instances, ~900 KB (the embedded SPA
bloated it) — on the host's **on-demand allocator** (this host build exposes no
pooling flag; `--max-components`/`--max-linear-memory-bytes` exist, an allocator
knob does not). Instantiating that per request ≈ 95 ms ⇒ ~10 rps, serialized at
`instances: 1`.

This is the SAME per-request-instantiation cost measured on the local native
host — where the default on-demand allocator gave 503 rps and **`--pool`
(wasmtime's pooling allocator, the strategy a production wasmCloud host uses)
took it to 2957 rps, ~6×**. The local pooling row is the proxy for what this
would do with pooling enabled.

**Three contributing causes, worst first — all addressable, none fundamental:**
1. **Per-request instantiation on the on-demand allocator** (~95 ms). Fix:
   pooling allocator (local `--pool` proved ~6×); a production wasmCloud host
   pools by default.
2. **Oversized fused component** (~900 KB / ~100 core instances) — the embedded
   SPA + fusing everything into one artifact inflate instantiation. Fix: serve
   the UI from a separate small static component (don't embed); the linked
   topology already splits the capabilities.
3. **Self-inflicted hot-path work — FIXED.** The domain re-seeded the i18n
   catalog + fsm machine (~15 wrpc-over-NATS hops) on EVERY request
   (`ensure_seeded()` in the handler). Free in-process (jco/native), brutal on
   the lattice — it even tripped the wrpc deadline (`data transmission timed
   out`). Now gated on a one-read KV check (seed once). This fixed the API
   paths; `GET /` was unaffected (it never seeded), which is exactly how the
   ~95 ms instantiation floor became visible.

So: the deploy works and serves everything; throughput is dominated by
per-request instantiation of an oversized component on an allocator-unoptimized
host.

### Making it faster — replica scaling, measured

The cheapest lever (no rebuild, no allocator change): scale the HTTP-facing
`vet-domain` to N instances so requests run in parallel instead of serializing
at `instances: 1`. Measured on the live cluster, GET / in-cluster:

| vet-domain instances | concurrency | req/s | vs baseline |
|--:|--:|--:|--:|
| 1 (baseline) | 20 | **16** | 1× |
| 5 | 50 | **~106–282** | **7–17×** |
| 10 | 100 | — (host thrash, requests stalled) | — |

`VET_REPLICAS=5 python3 gen-manifest.py` sets it. **Scaling to 5 gave a 7–17×
jump** — confirming the ~10 rps was `instances: 1` serialization, not a hard
limit. (The spread reflects single-host variance: 5 fat component instances + 18
capabilities + NATS all contend on one orbstack VM.) **10 over-saturated** the
single host (instantiation/memory thrash → stalls) — the per-request
instantiation cost is exactly what caps how many fat instances one host holds;
a real multi-node cluster, a pooling allocator, or a slimmer component all push
this further. Two compounding levers not run here: the **pooling allocator**
(~6× locally — cuts the per-instance ~50 ms instantiate) and **slimming the
component** (drop the embedded SPA). With pooling even `instances: 1` would beat
the scaled-but-on-demand number.

### 2. The shallow auth slice deploys + runs, but is provider-hop-bound
The **auth backend** (accounts-app + the composed auth-guard — a 2–3 component
graph, well under 30 instances) deploys cleanly: `register` → 201, `login` →
200 with real `sess_`/`ref_` tokens, persisted to NATS KV. Benched login:
**~5.4 req/s** — far below the local Rust host (165) or Node (38). That is **not
a runtime verdict**: it's dominated by

- a **per-KV-op round-trip to the `keyvalue-nats` provider** over NATS wrpc
  (argon2 login does several KV reads/writes; each is now a network hop vs the
  local in-memory store), plus
- `instances: 1` (a single host replica, no horizontal scaling), plus
- the `kubectl port-forward` in the measurement path.

So the wasmCloud number measures **durable networked KV + provider hops + single
replica**, the price of a real distributed deployment — orthogonal to the
host-runtime comparison above. To make it representative you'd scale
`spreadscaler` instances, run the load in-cluster (no port-forward), and note
that every persistence op is a deliberate network round-trip for durability.

### Takeaway
- For the **runtime** comparison, the local **pooling** Rust host is the right
  proxy for "what wasmtime can do" (it uses the same pooling allocator wasmCloud
  uses) — Node-class reads, multiples-better writes/auth, tunable memory.
- For a **real wasmCloud deploy**, two things bite before runtime does: the
  **composition-depth instance cap** (keep compositions shallow, or rebuild the
  host with a higher limit) and **provider round-trip latency** for durable KV
  (the cost of not being in-memory). Both are deployment-shape choices, not
  language or wasm-vs-native verdicts.

## Reproduce

```bash
# Rust host (add --pool for the pooling row):
just host-full            # on-demand
# edit the recipe / run directly with --pool for pooling

# Node/jco:
(cd examples/jco-vet-clinic && npm start)   # :3000

# load (seed + a token first):
oha -z 15s -c 50 -H "authorization: Bearer $TOKEN" http://127.0.0.1:PORT/pets
```
