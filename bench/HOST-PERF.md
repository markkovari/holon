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
