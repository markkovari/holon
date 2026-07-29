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

## Round 2 — the fixes, applied and measured

The three addressable causes above are now addressed in code/manifests:

### 1. SPA extracted into a `static-assets` component (`ui:assets`)
The React bundle is no longer `include_bytes!`-embedded in vet-domain — it is
its own pure component (`components/static-assets`, exports `ui:assets/files`)
that vet-domain links like any other capability. Artifact sizes:

| artifact | before | after |
|---|--:|--:|
| `vet_domain.wasm` (the HTTP-facing component) | ~940 KB (SPA embedded) | **289 KB** |
| `static_assets.wasm` (the SPA, linked) | — | 650 KB |

Per-request instantiation cost tracks component size on this host, so every
API request now instantiates a ~3× smaller component; the SPA bytes are only
touched on `GET /` / `/assets/*` (one wrpc hop to the static component — fine,
static is not the hot path). For the fused single-process hosts (jco/native)
`compose-vet`/`compose-vet-full` plug `static_assets.wasm` back in — verified
end-to-end on the native host: `GET /` = 200 html from the component,
register/login/pet/search green, SPA-fallback 200.

### 2. Hybrid fuse+link topology (`just compose-vet-lattice`, `LATTICE=1`)
The 30-nested-instance cap doesn't force all-or-nothing: fuse the 6
**pure-compute** caps (money, validate, md, pii, paginate, upload-policy) into
vet-domain and keep everything stateful linked:

| artifact | core modules | size |
|---|--:|--:|
| one capability (e.g. money) | 4 | — |
| `vet_domain.lattice.wasm` (6 caps fused) | **28** (< 30, deploys) | 816 KB |
| `vet_domain.full.composed.wasm` (19 fused) | 104 (rejected) | 3.4 MB |

Every fused call is now in-process (was: a full wrpc-over-NATS round-trip —
felt on POST /pets (validate), invoices (money), notes (md)). csv (admin
export, coldest path) stays linked to fit under the cap.
`LATTICE=1 VET_REPLICAS=5 gen-manifest.py` emits the matching manifest and
moves the fused caps' `wasi:config` knobs onto vet-domain.

### 3. Pooling — settled: not available on the v1 host line
`PoolingAllocationConfig` exists ONLY in the v2 `wash-runtime` crate — no v1
host release (1.4–1.9.2) uses wasmtime's pooling allocator (the v1.7/1.9
"pooling" notes are DB connection pools). So there is no host-version bump
that buys the ~6× allocator win; that requires migrating to **wasmCloud v2**
(wash-runtime + runtime-operator), which also does **in-process
component-to-component calls** — eliminating the wrpc/NATS hop for linked
components entirely. That migration is its own project (v1 capability
providers are gone in v2; keyvalue/http become built-in plugins).

### Round-2 results (measured)

Deployed on the `petclinic` stack (its own NATS/registry/wadm, host **1.6.2**,
lattice `petclinic`, listen `:8082` via `VET_ADDR`) — the original bench
namespaces were torn down, so this is same-machine (OrbStack) but not strictly
the same environment as round 1. In-cluster `oha` pod → host pod IP, no
port-forward. `LATTICE=1 VET_REPLICAS=1`:

| op | round 1 (linked, r1) | round 2 (hybrid lattice, r1) |
|---|--:|--:|
| GET / warm, single-shot | ~96 ms | **1.3–2.5 ms** |
| GET /me single-shot | — | **2.0 ms** |
| GET /pets single-shot | ~380 ms (1/2.6 rps) | **3–15 ms** |
| GET / (oha c=10, 10 s) | ~10–16 rps | **230 rps**, 100 % |
| GET /pets (oha c=10, 10 s) | ~2.6 rps | **67 rps**, 100 % |
| POST /pets sequential | — | 20 reqs < 1 s (≥ 20/s) |
| login sequential | ~5.4 rps (port-fwd) | ~10/s (argon2-bound) |

The ~95 ms per-request floor is **gone** — the 22–26× read-throughput jump
came from the slim artifact (no embedded SPA), the fused pure-compute hops,
host 1.6.2, and a clean single-app host (round 1's host was also serving
other lattices). GET / still pays exactly one wrpc hop (ui:assets) and does
230 rps on one instance.

**Two operational findings, both reproducible:**

1. **Concurrent write / argon2 load wedges the host's wrpc layer.** `oha -c 10`
   against POST /pets or /login stalls every request (oha reports NaN), and the
   host then hangs on ALL linked calls — including previously-fine reads —
   until the pod is restarted (reproduced 3×; zero ERROR lines in host logs;
   sequential writes are fast). Reads at c=10 do not trigger it. This, not
   memory, is likely what round 1 recorded as "host thrash" at 10 instances.
   Keep write concurrency low per host, scale hosts (not component instances)
   for writes, or move to the v2 runtime.

2. **With the slim artifact, replicas no longer pay on a single node.**
   `VET_REPLICAS=5`: GET / dropped to 68 rps @ c=20 and /pets wedged — the
   instantiation floor that replicas amortized in round 1 (7–17×) no longer
   exists, so extra fat instances on one node are pure contention. r1 is
   optimal on a single-node cluster.

Deploy recipe used (any stack): push the 16 images (`wkg oci push --insecure`),
apply a host CR with `natsAddress`/`allowedInsecure` for your namespaces, then
`LATTICE=1 VET_REPLICAS=1 VET_REG=… VET_NATS=… VET_ADDR=0.0.0.0:8082
python3 examples/vet-clinic-wasmcloud/gen-manifest.py | wash app put -` and
`wash app deploy vet-domain` (wash v1 / 0.43 — wash 2.x dropped `app`).

## Round 3 — wasmCloud v2 (wash-runtime + runtime-operator): four digits

The same components (rebuilt only for the `wasi:config` rename, see below) on
**wasmCloud v2** — `charts/runtime-operator` from wasmCloud/wasmCloud, CRD
group `runtime.wasmcloud.dev/v1alpha1`, installed side-by-side with the v1
operator. One `WorkloadDeployment` (`k8s/vet-domain-v2.yaml`): **16 components
linked IN-PROCESS in one wasmtime store** — no wrpc-over-NATS between
components, no wac fusing, no 30-nested-instance cap — with host plugins for
wasi:keyvalue (**NATS-backed, durable**), wasi:config, wasi:http (virtual-host
routing by Host header; the operator maintains a Service pointing at the host
pods). Engine runs wasmtime's **pooling allocator by default**.

Same machine, in-cluster oha → operator-maintained Service:

| op | v1 round 1 | v1 round 2 | **v2** |
|---|--:|--:|--:|
| GET / (static via ui:assets) | ~10 rps | 230 rps @ c10 | **1044 rps @ c50**, 100 % |
| GET /pets (auth + records) | ~2.6 rps | 67 rps @ c10 | **929 rps @ c50 / 1000 @ c100**, 100 % |
| login (argon2) | ~5.4 rps; c≥10 wedged the host | wedged | **349 rps @ c10**, 100 % |
| POST /pets (validate+write+index) | wedged | wedged | **820 rps @ c10**, 100 % |

~1000 rps is the CPU plateau of ONE hostgroup pod on this laptop, with every
KV op durable in NATS JetStream. The v1 concurrent-write wedge is gone.
Memory under load ~2.5 Gi (pooling reservation — the same trade the local
`--pool` host showed).

### Migration notes (what it actually took)
1. **`wasi:config` rename** — v2 serves `wasi:config/store@0.2.0-rc.1`; the
   components imported the older draft `wasi:config/runtime@0.2.0-draft`.
   Same functions, renamed interface + error type: vendored the rc.1 WIT,
   sed'd wit files + Rust paths, native host trait updated. (The jco examples'
   config shims still register the old name — update when touched.)
   `wasi:keyvalue@0.2.0-draft` and `wasi:http@0.2.0` bind unchanged.
2. **hostInterfaces are per-entry ALL-match**: an entry binds to a component
   only if the component's world covers every interface in the entry — list
   `wasi:keyvalue` `[store]` and `[atomics]` as separate entries or
   store-only components silently get no keyvalue and pre-instantiation fails.
3. **`poolSize`/`maxInvocations` on the HTTP-facing component** (defaults 0 =
   cold instantiation of the whole linked graph per call).
4. **Raise the hostgroup memory limit** — the chart default 512 Mi OOM-kills
   under the pooled 16-component chain (looked like "progressive collapse");
   4 Gi is comfortable. `--allow-insecure-registries` via `runtime.extraArgs`
   for an HTTP in-cluster registry.

Deploy: `helm install wasmcloud-v2 charts/runtime-operator …` then
`kubectl apply -f examples/vet-clinic-wasmcloud/k8s/vet-domain-v2.yaml`.

### Scaling out (hostGroups replicas + WorkloadDeployment replicas)

`runtime.hostGroups[0].replicas=3` (helm) + `spec.replicas: 3` on the
WorkloadDeployment → 3 workload replicas on 3 host pods, load-balanced by the
operator-maintained Service. Same laptop, in-cluster oha, 100 % success
(effectively ~2 pods serving — the third endpoint registered late):

| op | 1 replica | 3 replicas |
|---|--:|--:|
| GET / @ c100 | ~1044 | **1494 rps** |
| GET /pets @ c100 | ~1000 | **1422 rps** |
| POST /pets @ c30 | 820 @ c10 | **1860 rps** (9.2 ms mean) |
| login @ c30 | 349 @ c10 | **1105 rps** |

POST at 1860 rps now beats even the local single-process pooling host
(1793). Sub-linear scaling here is just shared laptop cores + one NATS —
on real multi-node hardware this is the shape that goes linear. Extrapolated:
10k rps ≈ 8–12 host pods.

**Helm gotcha:** `--set runtime.hostGroups[0].replicas=3` REPLACES the whole
array element — the default `http.port: 9191` is silently dropped, hosts come
up with no HTTP listener (Host CR shows `httpPort: 0`), and the route
controller deletes every Service endpoint. Re-specify
`runtime.hostGroups[0].http.enabled=true` + `.http.port=9191` (or use a
values file) whenever touching `hostGroups`.

### The hosts × replicas sweep (external load generator over SSH/tailscale)

Load from a second machine (`oha` on another Mac → tailscale → socat relay →
Service ClusterIP, `Host:` header routing; the path adds ~16 ms RTT, so c=30
runs cap near ~1100 rps regardless of the app — compare columns, not to the
in-cluster numbers):

| hosts × replicas | GET /pets c100 | POST /pets c30 | login c30 |
|---|--:|--:|--:|
| 5 × 10 (pool 8) | **1284 rps** | 929 | 1001 |
| 10 × 30 (pool 2) | 713 | 884 | collapsed (timeouts) |
| 10 × 100 (pool 1) | 838 | 665 | 351 |

- **100 replicas place and run fine**: 100 workloads × 16 components =
  **1600 component instances** across 10 host pods, node idle at 5 % CPU /
  3.4 Gi when unloaded — density is a non-issue (that was always wasmCloud's
  pitch; v2 delivers it with in-process linking).
- **Throughput does NOT come from replicas on one machine**: the 10-core
  laptop is a fixed pie; past ~3 hosts × 3 replicas every extra shard adds
  scheduling/memory overhead and cuts the warm-pool budget (pool 8 → 1),
  so rps *drops*. The sweet spot here stayed 3 × 3 (~1400–1900 rps
  in-cluster). Replicas buy throughput only when they land on new hardware —
  on a real cluster, hosts-per-node ≈ 1 and replicas ≈ node count.

**Bottom line across the three rounds:** 10 rps → 230 rps → **1000+ rps**
(single host) → **1400–1900 rps** (3 replicas, one laptop) for
the identical application wasm; 100-replica density verified, and further
throughput requires more machines, not more shards. Round 1's cost was deployment shape (fat
artifact, per-request instantiation, `instances: 1`), round 2 removed the
shape mistakes within v1's limits, and v2 removes the architecture tax
(in-process linking + pooling + real concurrency control), leaving ordinary
CPU as the ceiling — scale hostgroup replicas from here.

## Round 4 — the apples-to-apples ladder (wasm vs native, layer by layer)

"If there are no network hops, why isn't it native-fast?" Answered with
`components/bench-suite` (a wasm component on wasmCloud v2, 7 endpoints, each
adding ONE layer) vs `host/src/bin/refbench.rs` (the same /ok, /json, /echo as
a plain hyper binary — no wasm). Same in-cluster oha, same laptop, c=50, 10 s:

| rung | native hyper | wasm on wasmCloud v2 | the layer costs |
|---|--:|--:|---|
| /ok (bare 200) | **93 488 rps** @ 0.53 ms | **34 670 rps** @ 1.44 ms | runtime+router tax ≈ **0.9 ms/req** |
| /json (+serde ser) | 91 k-class | 33 907 rps @ 1.47 ms | ser ≈ free |
| /echo (+serde deser) | 91 422 rps @ 0.54 ms | 31 384 rps @ 1.59 ms | deser ≈ 0.1 ms |
| /kv-read (+1 kv get) | — | 9 987 rps @ 5.0 ms | NATS kv open+get ≈ **3.4 ms** |
| /kv-rw (+kv set) | — | 9 134 rps @ 5.5 ms | +set ≈ 0.5 ms |
| /blob-read (1 KiB) | — | 248 rps @ 19.5 ms (single-shot 1.4 ms) | plugin serializes ~4 ms/op |
| /blob-rw (1 KiB) | — | single-shot ~2 ms; **do not bench concurrently** | see bug 2 |

(mac-loopback native is 212 k rps / 0.23 ms; the in-cluster figures above are
the same-path comparison. The bench pod itself is a chunk of the 10 cores in
both columns.)

**So the answer:** the wasm stack IS relatively close to native on pure
compute — **~1/3 of hyper, ~34 k rps, with a fixed ~0.9 ms invocation+
virtual-host-router tax** and serde effectively free. What separates the
vet-clinic app (~1 k rps) from 34 k is not the runtime: it's **backend
round-trips** — every `wasi:keyvalue` op is a real NATS JetStream trip
(~3.4 ms for open+get), and a /pets request does several (seed gate, session
lookup, records list, index). That cost is the price of durable, shared state
and exists for the native framework too the moment it talks to a database.

**Two upstream wash-runtime bugs found:**
1. `NatsBlobstore::incoming_value_consume_sync` reads into a zero-capacity
   buffer and always returns empty — use `consume-async` (streamed) instead.
2. Concurrent `write-data` to the same object wedges the host's invocation
   path until pod restart (same silent-wedge family as the v1 concurrent-write
   stall). Sequential blob writes are fine (~2 ms).

Reproduce: deploy `examples/vet-clinic-wasmcloud/k8s/bench-suite-v2.yaml`
(note the `buckets:` allow-list on the blobstore hostInterface), run
`cargo run --release --bin refbench` in `host/`, and point oha at both.

## Round 5 — the KV-trip diet + platform limits (v2, 0.2.1 images)

Three app changes (see the 0.2.1 components): `ensure_seeded()` gated to the
fsm/i18n routes instead of every request; record-store list/find-by/query
fetch pages via `wasi:keyvalue/batch.get-many` (ONE backend round-trip — the
wash-runtime NATS plugin runs the gets concurrently; falls back to per-key
gets on hosts without batch); auth-guard RBAC collapsed to one doc per tenant
(`rbac:{tenant}`), so authorize = session get + 1 kv read. GET /pets dropped
from ~11 KV ops to ~4. NOTE: components now IMPORT `wasi:keyvalue/batch` —
the v2 manifest needs its own `interfaces: [batch]` hostInterfaces entry, the
native host + jco shims got batch impls, and the RBAC key layout changed
(re-seed roles/perms on deploy).

Two PLATFORM ceilings surfaced once the app got faster, both fixed:

1. **wasmtime pooling `total_core_instances` (default 1000) per host** — every
   non-pooled invocation instantiates the ~28-core-module chain, and pool
   slots count against the same budget (poolSize 48 × 28 = 1344 starved the
   engine outright → 25-35 % 500s at c≥20). Fix: wash-runtime reads
   `WASMTIME_POOLING_TOTAL_*` env — set `TOTAL_CORE_INSTANCES=8000` (+
   MEMORIES/TABLES=8000, STACKS/COMPONENT_INSTANCES=2000) on the hostgroup
   deployment. (Env lives on the Deployment, NOT in helm values — a helm
   upgrade wipes it.)
2. **NATS at chart defaults: 128Mi limit + `-DV` payload-trace logging.**
   Concurrent writes OOM-killed JetStream (verified OOMKilled), wedging every
   host's kv client for minutes (`JetStream TimedOut` storms; the Service
   loses endpoints while replicas recycle) — this masqueraded as the app
   "write wedge". Fix: drop `-DV`, requests 500m/512Mi, limits 2/2Gi.
   JetStream storage is emptyDir, so the NATS restart also reset the bucket.

Final ladder (3 hostgroup pods × 3 workload replicas, poolSize 16,
maxInvocations 1000, in-cluster oha, fresh bucket, 10 s runs — ALL 100 %):

| op | round 3 (1 rep) | round 3 (3 rep) | **round 5 (3 rep)** |
|---|--:|--:|--:|
| GET / @ c50 | 1044 | 1494 | **1823 rps** @ 27 ms |
| GET /pets @ c50 | 929 | — | **1266 rps** @ 40 ms |
| GET /pets @ c100 | ~1000 | 1422 | **1068 rps** @ 94 ms |
| POST /pets @ c10 | 820 | — | **459 rps** @ 22 ms |
| POST /pets @ c30 | — | 1860 | **558 rps** @ 54 ms |
| login @ c10 / c30 | 349 | 1105 | **170 / 186 rps** |

Reads: best numbers this cluster has produced, zero errors up to c100.
Writes trail round 3's peaks: the id-index RMW grows O(N) *during* the run
(5.5k creates in 10 s), and login is argon2-CPU that `maxInvocations: 1000`
now funnels onto fewer pooled instances (round 3's 200 spread it wider) —
lower maxInvocations to spread CPU-bound paths, or accept it; the honest
write ceiling is the O(N) id-index + per-term posting-list RMW in
record-store, which is the next component-side lever (chunked/sharded
indexes). GET /pets?limit=1 at 1.7k owner records was also observed at 20 s
(fetch-all-then-sort pagination + the batch fallback going sequential when
one get errors) — index-ordered pagination is the fix.

### Round 5b — chunked indexes (records-store 0.2.2)

Both levers applied. Every id list in record-store (the per-collection id
index AND every secondary index) is now a small manifest + sorted chunks of
≤1024 ids (`idx_…` -> manifest, `idx_…_c{seq}` -> chunk): inserts/removes
touch one ~30 KB chunk + the manifest (written in one `set-many`) instead of
an O(N) RMW of one unbounded value (which also had a hard wall at NATS's
1 MiB message cap ≈ 33k ids). `list-records` pages by fetching only the
chunks the page touches; `count` is one manifest read; a legacy whole-array
value is read transparently and split into chunks on its first write
(verified live against the 0.2.1 bucket). The get-many sequential fallback is
GONE — a batch error now propagates instead of degrading into N serial gets
(the actual mechanism behind the 20 s page).

Measured on the same bucket grown to ~10–20k pets (in-cluster oha, 100 %):

| op | 0.2.1 (whole-array) | 0.2.2 (chunked) |
|---|--:|--:|
| GET /pets?limit=1, ~10k-pet owner | 20 s | **217 ms** |
| single POST @ ~10k records | 40 ms (worsening) | **15–17 ms (flat)** |
| POST /pets @ c10 / c30 | 459/558 fresh, ~260 at 10k | **319 / 348 rps, size-independent** |
| GET /pets @ c50 (3-pet user) | 1266 | 901 (secondary ix now 2 reads; cluster noise) |

Remaining O(N) writer: search-index term posting lists (a hot term's list
still RMWs whole — same chunking recipe applies if it ever shows up outside
same-name bench data). The ?limit page still *fetches* all owner records
(one batched trip) before the name-sort — the 217 ms is that fetch+parse;
a name-sorted index would be the next step only if a real UI needs faster.

## Round 6 — second hardware + the hybrid lattice

### 6a. Same stack, second machine (csatapaci: 12-core Mac, 96 GiB OrbStack)

Full clean install (chart from wasmCloud/wasmCloud main = 2.5.1) with the
round-5 lessons folded into VALUES this time: `runtime.env` carries the
`WASMTIME_POOLING_TOTAL_*` knobs, `nats.resources` set 500m/512Mi–2/2Gi, and
the chart's hardcoded `-DV` NATS arg deleted from the template (upstream bug:
`templates/nats/deployment.yaml` payload-trace-logs every message; it is what
OOM'd NATS at the default 128Mi in round 5). Fresh bucket, 3 hosts × 3
replicas, in-cluster oha, ALL 100 %:

| op | picur (10c, round 5b) | **csatapaci (12c)** |
|---|--:|--:|
| GET / @ c50 | 1823 | **1926 rps** |
| GET /pets @ c50 / c100 / c300 | 1266 / 1068 | **829 / 1650 / 1742 rps** (plateau c200+) |
| POST /pets @ c10 / c30 | 459* | **605 / 684 rps** |
| login @ c30 | 186 | **257 rps** |

(*picur POST was on a 10k-record bucket; csata fresh — but chunked indexes
make that comparison nearly moot now.) Roughly +20–40 % over picur, in line
with 12 vs 10 cores; the linear-with-hardware story holds.

### 6b. Bare-metal host joins the k8s operator (the open v2 question: YES)

A plain `wash host` (the ghcr.io/wasmcloud/wash:2.5.1 image under Docker on
csatapaci — NO k8s on that side) joined picur's operator over the LAN:
kubectl port-forwards exposed picur's NATS (:4222) and registry (:5555→5000)
on the LAN; the container got `--add-host` aliases for the NATS cert SAN +
registry name, the client TLS certs from picur's `wasmcloud-runtime-tls` /
`wasmcloud-data-tls` secrets, and the same `--host-group default
--environment wasmcloud-v2`. Result: a Host CR appears in picur's cluster,
the operator SCHEDULES vet-clinic replicas onto it, the remote host pulls all
16 images from picur's registry and serves: GET / 144 ms, login OK, /pets
0.7–0.9 s (the KV-per-op WAN tax — a JetStream leaf node on the remote side
is the fix for real deployments). So: v2 orchestration needs *a* k8s for the
operator, but hosts are location-independent — the "tiny managed brain +
rented metal muscle" shape works today.

Findings for upstream: (1) scheduler places workloads onto STALE Host CRs —
a restarted host leaves a ghost CR for ~90 s and new workloads pin to it,
stuck Ready=False until the reaper runs, then reschedule cleanly; (2)
`--enable-meters` enables wasmtime FUEL metering and the 28-core-module
vet-clinic chain traps with "all fuel consumed" on instantiation — do not
enable it for composed apps (and it didn't populate Host CR system metrics
anyway; those read 0 for in-cluster pods too); (3) workload spread is
concentration-heavy (12 replicas landed 5/5/1/1 across 4 hosts).

### 6c. The two Macs serving together — hybrid lattice under load

Same topology under load: picur's 3-pod cluster + the bare csatapaci host in
ONE lattice (18 replicas; the remote held 3), loaded simultaneously — oha in
picur's cluster → Service, oha in a container on csatapaci → the metal host
directly. The remote host needs the SAME `WASMTIME_POOLING_TOTAL_*` env as
the pods (its 2–3 workloads × poolSize 16 × 28 modules ate the default
1000-core budget: static traffic survived on fast turnover, /pets threw ~95 %
500s until the env was set — round 5's lesson applies per host, everywhere).

| path | rps | success |
|---|--:|--:|
| picur cluster GET / @ c50 (during combined) | 2006 | 100 % |
| csatapaci metal GET / @ c50 (during combined) | 805–821 | 100 % |
| **combined static, one lattice, two Macs** | **~2800 rps** | 100 % |
| csatapaci metal GET /pets @ c20 | **98 rps @ 206 ms** | 100 % |
| (picur in-cluster /pets, for contrast) | 1266 @ 40 ms | 100 % |

The two-line summary of hybrid physics: **compute paths scale across
machines almost additively; KV paths divide by data locality.** The remote
host's every `wasi:keyvalue` op crosses the LAN bridge to picur's JetStream
(and all its requests multiplex ONE NATS client connection), so the KV-heavy
path runs at ~8 % of local speed. Production hybrid = JetStream leaf node (or
domain) on the muscle side; bridge transport barely matters otherwise (socat
vs `kubectl port-forward` measured identically once pooling was fixed).
Footnote: the virtual-host router alone (remote host, no workload → 404)
clocked ~115k rps — routing is never the bottleneck.

### 6d. Streaming probe — SSE through wash-runtime: STREAMS

`bench-suite:0.1.4` adds `GET /sse`: 10 events, one `blocking_write_and_flush`
every 200 ms, `ResponseOutparam::set` before the first write. Measured
in-cluster with a raw-socket client stamping each chunk's arrival: events
landed at +0.00 s, +0.21 s, … +1.83 s — individually, as flushed, headers
first. wash-runtime's `wasi:http` path does incremental delivery, so
LLM-token streaming (SSE) works on wasmCloud v2 as-is. (This was the go/no-go
check for AI-harness workloads.)

## Round 7 — Raspberry Pi 5 (malna): the native host on €80 hardware

Same native Rust host + `vet_domain.full.composed.wasm`, cross-compiled from
the Mac with `cargo zigbuild --target aarch64-unknown-linux-gnu.2.36` (~70 s
build, 15.7 MB binary — scp'd with the 3.4 MB wasm + 620 KB SPA, nothing else
installed on the Pi). malna = Pi 5 Model B, 4× Cortex-A76, 8 GiB, Debian
bookworm. Loaded from the Mac over LAN (oha, ~1.8 ms RTT), in-memory KV,
`oha -z 15s`, ALL rows 100 %:

| op | on-demand | **pooling (`--pool`)** | picur Mac pooling (round 1) |
|---|--:|--:|--:|
| GET / (static) @ c50 | 2871 rps | **7653 rps @ p50 7 ms** | — |
| GET /pets @ c50 | 1083 @ 46 ms | **1428 rps @ p50 35 ms** | 2957 |
| POST /pets @ c10 | 660 @ 14 ms | **802 rps @ p50 12 ms** | 1793 |
| POST /login @ c10 | 27 | **30 rps @ p50 349 ms** | 175 |
| RSS after load | 134 MB | 141 MB | 427 MB |

Reads and writes land at ~half the M-series Mac — in line with 4 small cores
vs 10 big ones; the linear-with-hardware story extends down to a Pi. The two
outliers tell the usual story: GET / never touches wasm (native static-file
path — the Pi pushes 7.6k rps of SPA without breaking p50 7 ms), and login is
argon2 — ~350 ms per hash on a Cortex-A76 vs ~30 ms on the M-series, so the
Pi does 30 rps of logins and nothing will fix that but cheaper KDF params or
bigger cores. Pooling's win over on-demand is smaller than on the Mac
(+32 % on /pets vs +490 % round 1) — instantiation is a smaller slice of the
budget when every core is slow and the KV work dominates.

Takeaway: the whole vet-clinic — 28 wasm modules, UI + API, one binary — runs
a four-digit read path on a Raspberry Pi at 141 MB RSS. A JetStream leaf node
on the Pi (nats-server is already installed) is the missing piece to make it
a real lattice edge node rather than an island.

## Round 8 — WASI p3 (async components): the compute rungs, ported

`components/bench-suite-p3` is the bench ladder's three compute rungs (/ok,
/json, /echo) rebuilt as a **p3 async component**: exports
`wasi:http/handler@0.3.0`, handler is an `async fn(Request) ->
Result<Response, ErrorCode>`, bodies are native `stream<u8>` — no wasi:io, no
outparams, and **one instance serves concurrent requests natively** (no
poolSize / maxInvocations / `WASMTIME_POOLING_TOTAL_*` arithmetic).

**"p3 enabled by default" — settled (July 2026, wasmCloud main):** the April
2026 blog's `wasip3` Cargo feature is GONE — wash-runtime now builds with
wasmtime's `p3` + `component-model-async` features unconditionally (wasmtime
pinned to a git rev ahead of the stock releases; upstream flips the default in
wasmtime 46). There is no config toggle either (`dev.wasip3` no longer
exists): dispatch is auto-detected from the component's exported world
(`handler@0.3.0` → p3 path, `incoming-handler@0.2.0` → p2 path). Released
wash 2.5.1 images predate this; wash **2.5.2+ / main** is the p3-by-default
line. WIT versions are final `@0.3.0` (the blog's `0.3.0-rc-2026-03-15`
suffix is gone).

Toolchain that works (mirrors wasmCloud main's own fixtures/examples):
`wit-bindgen 0.58` with `async-spawn` + `inter-task-wakeup`, plain cdylib
built to `wasm32-wasip2` (no wasip3 rustc target yet — the p3-ness lives in
the WIT), p3 WIT deps vendored from wasmCloud's
`crates/wash-runtime/tests/fixtures/p3-wit-deps`. Standalone crate — the
components workspace stays on cargo-component/wit-bindgen-rt 0.41, which
can't emit async bindings. libstd's `wasi:cli/io@0.2` imports ride along in
the built component; the host serves both generations to one component.

Measured under `wash dev` (main = 2.5.2), same laptop, loopback oha, 10 s,
ALL 100 %; the p2 column is wasmCloud's own `http-handler-p2` fixture (static
body ≈ the /ok floor) on the SAME wash binary:

| op | p3 (1 instance) | p2 (same host) |
|---|--:|--:|
| GET /ok @ c50 | **27.0–28.0k rps @ 1.8 ms** | 33.3k rps @ 1.5 ms |
| GET /ok @ c200 | **26.8k rps @ 7.5 ms** (flat) | 33.3k rps @ 6.0 ms (flat) |
| GET /json @ c50 | 26.9k rps | — |
| POST /echo @ c50 | **26.6k rps** — writes at read speed | — |
| RSS after load | ~55–69 MB | (same) |

Takeaways:
- **p3 works end-to-end on stock main with zero flags** — build the world,
  wash dev serves it down the p3 dispatch path.
- **The floor costs ~19 % vs p2 today** (27k vs 33.3k) — the async plumbing
  (per-response wit_stream + trailers future + spawn) isn't free and the
  impl is preview-grade; p3's pitch here isn't raw floor rps.
- **What p3 actually buys:** concurrent POSTs run at read speed on one
  instance (the p2 concurrent-write wedge family doesn't apply), and
  concurrency needs no pool-slot budgeting — c200 is flat with no
  `total_core_instances` math.
- **Migration blocker unchanged:** only wasi:http has a user-facing p3 path.
  wasi:keyvalue/config stay p2 (main has `wasmcloud:keyvalue@0.1.0` p3
  fixtures — watch that), so the vet-clinic app can't follow yet.
- Upstream nit found: the `http-handler-p2` fixture's `wkg.toml` points at
  `p3-wit-deps` paths that don't exist — delete it and `wkg wit fetch` to
  build.

Reproduce: `cargo build -p wash --release` from wasmCloud main, then
`wash dev` in `components/bench-suite-p3` (`.wash/config.yaml` sets
`dev.address`), `oha -z 10s -c 50 http://127.0.0.1:8000/ok`.
## Round 9 — link-shortener: a small composition's ceiling is the allocator

`components/link-shortener` is the smallest "real app" composition in the
catalog: an HTTP component plugging **slug + id-generate + record-store +
rate-limiter + cache(+cache-backing)** — 7 components in the composed graph vs
vet-clinic's 19+. Its redirect hot path is deliberately thin: one cache read,
one atomic click increment, a 302. The question: does a small graph move the
per-request-instantiation wall from Rounds 2/4?

Native `comp-host`, `--kv memory`, same laptop, loopback oha, 10 s per row,
100 % success everywhere (no non-2xx/3xx):

| route | on-demand | `--pool` |
|---|--:|--:|
| GET /{code} (redirect: cache read + atomic bump + 302) @ c50 | 2.35k rps @ 21.4 ms p50 | **17.5k rps @ 2.9 ms p50** (p99 5.1 ms) |
| GET /{code} @ c200 | 2.39k rps @ 81.7 ms | **17.5k rps @ 11.3 ms** (flat) |
| GET /api/links/{id} (record get + click counter) @ c50 | 2.34k rps | 16.0k rps |
| GET / (static JSON, no KV at all) @ c50 | 2.36k rps | 17.1k rps |
| host RSS after load | — | ~62 MB |

Takeaways:
- **On-demand, every route is 2.35k** — including the root route that touches
  no KV and no capability. The route body is invisible; per-request
  instantiation of the 7-module graph is the whole bill. Same wall as the
  19-component vet-clinic hit, barely moved by being 2.7× smaller.
- **Pooling is 7.4×** (2.35k → 17.5k @ c50) and stays flat at c200 with p99
  under 25 ms. Confirms Round 2's conclusion at the small end of the graph-size
  axis: the pooling allocator is not an optimization, it's the difference
  between a demo and a service.
- **The composition itself is ~free.** Pooled, the full redirect path (cache
  get through the composed cache component + `wasi:keyvalue/atomics` bump +
  302) runs at 17.5k vs the do-nothing floor's 17.1k — within noise. The stats
  route's extra record-store get + counter read costs ~9 % (16.0k). Cross-
  component calls in a wac-fused graph are effectively function calls.
- Rate-limited create (`POST /api/links` behind ratelimit:guard) was left out
  of the ladder on purpose — a create-flood benches the guard's 429 path, not
  the app.

Reproduce: `just host-shortlink` (add `--pool` to the recipe's flags for the
pooled row), seed one link, then
`oha -z 10s -c 50 http://127.0.0.1:3008/{code}`.

## Round 10 — dev-portal: auth + ABAC + quota on every request, still ~free

`components/dev-portal` is the control-plane app from PLATFORM.md: projects,
sha256-hashed API keys, a metered gateway, and stripe-signed webhook delivery
off a durable outbox. Its composed graph is **9 components** (the composed
auth-guard bundle + record-store + id-generate + quota + policy-guard +
outbox + webhook-sign + notify-dispatch) — the biggest app graph benched
since vet-clinic, and unlike the link shortener every interesting route does
REAL cross-component work: bearer introspection (auth-guard), ABAC rule
evaluation (policy-guard), or an atomic quota reserve.

Native `comp-host`, `--kv memory`, same laptop, loopback oha, 10 s per row,
100 % success (all 200s — the bench key's limit is 10⁹ so quota never trips):

| route | on-demand | `--pool` |
|---|--:|--:|
| POST /api/gateway/echo (sha256 + indexed key lookup + quota reserve) @ c50 | 1.41k rps @ 35.7 ms p50 | **10.5k rps @ 4.8 ms p50** (p99 8.5 ms) |
| POST /api/gateway/echo @ c200 | — | **10.5k rps @ 18.6 ms** (flat) |
| GET /auth/me (authorizer introspect) @ c50 | 1.42k rps | 11.1k rps |
| GET /api/projects/{id} (introspect + record get + ABAC eval) @ c50 | 1.40k rps | 10.4k rps |
| GET / (static JSON floor) @ c50 | 1.43k rps | 11.2k rps |
| host RSS after load | — | ~99 MB |

Takeaways:
- **The graph-size axis, quantified.** On-demand floors: 7 components → 2.35k
  (Round 9), 9 components → 1.4k. Instantiation cost scales with the composed
  graph, and it stays the whole bill until pooling: pooled, both apps land at
  their own flat ceiling (17.5k vs 10.5–11.2k). Same 7.4–7.5× pooling ratio at
  both sizes.
- **Security is not the tax.** Pooled, the full gateway path — hash the
  presented key, indexed find-by, atomic quota reserve — runs within ~6 % of
  the do-nothing floor (10.5k vs 11.2k). A bearer introspect through the
  composed auth-guard is ~1 % off floor (11.1k). The two-layer authorize
  (introspect + record get + policy:guard rule eval) costs ~7 % (10.4k).
  Cross-component contract calls keep behaving like function calls.
- The pooled floor itself (11.2k vs Round 9's 17.1k) shows the per-request
  cost that DOES grow with graph size even pooled — more modules per
  instantiation slot to wire, ~35 % floor drop for +2 components and the much
  larger auth-guard bundle.
- Write-path routes (mint key, drain outbox with live HTTP delivery) were
  left out: minting floods the KV with records and drain benches the
  receiver, not the portal.

Reproduce: `just host-portal` (add `--pool` for the pooled row), register +
login + create project + mint a high-limit key, then
`oha -z 10s -c 50 -m POST -H "x-api-key: dk_…" http://127.0.0.1:3009/api/gateway/echo`.

## Round 11 — three apps, one axis: relay, ledger, status-page

Three new apps land the composition patterns the catalog had not exercised:
**webhook-relay** (`relay:app`, 10 modules composed: webhook-ingest(+idempotency-guard)
+ jsonpatch + outbox + webhook-sign + notify-dispatch + rate-limiter + audit-log
+ record-store) is the reliability trio end to end — HMAC ingest, replay dedup,
durable outbox, github-signed delivery with retry + dead letters. **billing-ledger**
(`ledger:app`, 7 modules: money + record-store + idempotency-guard + quota + csv
+ outbox) is the Stripe-style `idempotency-key` write path over integer-minor-unit
money. **status-page** (`status:app`, 6 modules: scheduler-timer + record-store +
fsm-workflow + event-bus + notify-dispatch) is the first TIMER-driven app — work
originates from `sched:timer`, not a request. Together they extend the graph-size
axis of Rounds 9/10 with three more points: 6, 7, and 10 modules.

Native `comp-host`, `--kv memory`, same laptop, loopback oha, 10 s per row,
100 % success everywhere:

**webhook-relay** (:3010)

| route | on-demand | `--pool` |
|---|--:|--:|
| POST /hook/{id} replay (kv secret + HMAC verify + idem lookup) @ c50 | 1.65k rps @ 30.3 ms p50 | **11.5k rps @ 4.3 ms p50** (p99 7.8 ms) |
| POST /hook/{id} replay @ c200 | 1.67k rps @ 116.9 ms | **11.6k rps @ 16.8 ms** (flat) |
| GET /api/sources (record list) @ c50 | 1.64k rps | 11.4k rps |
| GET / (static JSON floor) @ c50 | 1.68k rps | 12.4k rps |
| host RSS after load | — | ~82 MB |

**billing-ledger** (:3011)

| route | on-demand | `--pool` |
|---|--:|--:|
| POST entries, fixed idempotency-key (cached replay) @ c50 | 2.29k rps @ 21.9 ms p50 | **18.0k rps @ 2.8 ms p50** (p99 5.1 ms) |
| POST entries, fixed idempotency-key @ c200 | 2.25k rps @ 87.5 ms | **18.5k rps @ 10.6 ms** (flat) |
| GET /api/accounts/{id} (record get + money format) @ c50 | 2.23k rps | 17.5k rps |
| GET statement.csv (find-by + csv over 5 entries) @ c50 | 2.15k rps | 16.4k rps |
| GET /api/allocate (pure money math) @ c50 | 2.17k rps | 18.4k rps |
| GET / (static JSON floor) @ c50 | 2.19k rps | 18.3k rps |
| host RSS after load | — | ~62 MB |

**status-page** (:3012, 5 monitors seeded)

| route | on-demand | `--pool` |
|---|--:|--:|
| GET /api/status (record list + 5× fsm get-status) @ c50 | 2.60k rps @ 19.3 ms p50 | **17.2k rps @ 2.9 ms p50** (p99 5.2 ms) |
| GET /api/monitors/{id}/history (fsm transition log) @ c50 | 2.57k rps | 19.9k rps |
| GET /api/events (bus consumer-group poll) @ c50 | 2.68k rps | 19.7k rps |
| GET / (inline html floor) @ c50 | 2.62k rps | 20.0k rps |
| host RSS after load | — | ~64 MB |

Takeaways:
- **The graph-size axis now has five points.** On-demand floors: 6 modules →
  2.6k, 7 → 2.2k, 7 (shortlink, Round 9) → 2.35k, 9 (portal, Round 10) → 1.4k,
  10 (relay) → 1.65k. Pooled floors: 20.0k / 18.3k / 17.1k / 11.2k / 12.4k.
  Module COUNT is not the whole story — the 10-module relay outruns the
  9-module portal on both floors because the portal's auth-guard bundle is a
  much bigger module (argon2 + JWT crypto) than the relay's ten small ones.
  Instantiation cost scales with total compiled size, not import count.
- **The ledger's 7-module numbers reproduce Round 9's 7-module numbers**
  (2.2k/18.3k vs 2.35k/17.1k floor) on a different app with different
  capabilities. The wall is structural, not app-specific.
- **An idempotent replay costs nothing.** The cached-response write path (idem
  begin → cache hit → replay the stored 201) runs AT the floor (18.0k vs
  18.3k) — the whole point of the pattern: retries don't re-execute money
  arithmetic, quota, records, or outbox, they cost one kv read.
- **The relay's verify path is ~7 % off floor** (11.5k vs 12.4k) for a kv
  secret fetch + HMAC-SHA256 + dedup lookup through two composed layers
  (webhook-ingest → idempotency-guard). Signature checking as a composed
  contract, ~free at 11.5k rps.
- **status-page's fan-out read costs ~14 %** (17.2k vs 20.0k) for a record
  list + five fsm get-status reads — six kv round trips per request. The
  history and bus-poll routes (one kv read each) sit at floor.
- Excluded on purpose: the relay's ACCEPT path and the drains (oha can't mint
  a unique delivery-id/idempotency-key per request, and accepted events grow
  the outbox unboundedly while drains bench the receiving server, not the
  app); status-page's `POST /api/tick` (probes external targets on a timer
  cadence — the bench would measure the probe target).

Reproduce: `just host-relay` / `host-ledger` / `host-status` (add `--pool` to
the recipe's flags for the pooled rows), seed via the routes in each app's
`GET /`, then e.g.
`oha -z 10s -c 50 -m POST -d '{"order":42}' -H "x-relay-signature: <hex>" -H "x-relay-delivery: bench-1" http://127.0.0.1:3010/hook/{id}`.

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
