# eshop bench + component-DX evaluation

The eShopOnDapr recreation (ESHOP.md) measured on the orbstack cluster
(2026-07-19: ns `eshop`, runtime-operator chart v2.5.2, wash-runtime 2.5.2,
1 hostgroup pod, 1 replica per workload, NATS JetStream KV, laptop). Method:
in-cluster `oha` pods against the operator-maintained Services — same as
HOST-PERF rounds 3–5, so numbers are comparable to the vet-clinic stack.

## HTTP rungs (10s each, 100% success on every rung)

| rung | path | c | rps | p50 | p99 |
|---|---|--:|--:|--:|--:|
| SPA serve (gateway, embedded bytes) | `GET /` | 50 | **18 622** | 0.3 ms | 41.8 ms |
| catalog browse, direct | `GET /api/catalog/items?pageSize=10` | 50 | **2 865** | 17.3 ms | 24.2 ms |
| catalog browse, via gateway proxy | same, through `gateway` | 50 | **2 202** | 21.9 ms | 37.5 ms |
| login (argon2) | `POST /login` (identity) | 10 | **184** | 53.9 ms | 75.5 ms |
| basket read (introspect + record) | `GET /api/basket` | 20 | **3 319** | 5.9 ms | 9.7 ms |
| orders list, via gateway | `GET /api/orders` | 20 | **1 883** | 10.3 ms | 18.2 ms |

Readings:
- The wasm **reverse proxy costs ~23% rps / +4.6 ms p50** — an acceptable
  Envoy stand-in for a demo; a real deployment would put an Ingress there.
- KV-backed reads land 2–3.3k rps — **~3× the vet-clinic round-3 plateau**
  (same laptop): smaller per-request KV fan-out + wash-runtime 2.5.2.
- login matches vet's argon2-bound ~170–186 rps. CPU, not architecture.

## Checkout choreography (the eShop-specific number)

50 serial checkouts from one buyer, then pump-until-paid (grace window 10s):

- submit: 50 checkouts in 4 s (PUT basket + POST checkout each)
- **47/50 orders reached `paid`; total wall 32 s** (10 s of that is the
  deliberate grace window), drain ≈ **1.8 orders/s** with a single pump driver
- stock decremented 48 for 47 paid orders

The three "missing" orders and the extra decrement are *findings, not
failures*:

1. **Basket-clear race (3 × 400)** — the serial one-buyer loop raced the
   `OrderStarted → clear basket` consumer: the clear landed between an
   iteration's PUT and checkout, so checkout correctly answered
   `400 basket is empty`. Real buyers don't checkout 50×/4s from one basket;
   the per-order snapshot in `UserCheckoutAccepted` means accepted orders are
   never corrupted.
2. **One duplicate stock decrement** — two concurrent pump drivers (the
   standing `eshop-pump` + the bench loop) polled the same unacked
   `OrderStatusChangedToPaid` batch; `event:bus` is at-least-once and the
   catalog's decrement is not idempotent per event. The original eShop has the
   same semantics over Dapr pub/sub. **RESOLVED** (follow-up commit):
   `idempotency:guard` is now composed into both non-idempotent consumers —
   catalog (keyed per order) and ordering's order creation (keyed per bus
   event id; a naïve retest showed duplicate *deliveries* minting duplicate
   *orders*, which per-order keys can't see). Exact-once verified under
   deliberately racing pumps: 20 checkouts → exactly 20 orders / 20 paid /
   20 decrements. Watermark caveat encoded too: `event:bus` ack advances past
   everything below the highest id, so consumers process in order and stop at
   the first skippable event.
3. **Drain rate is KV-op-bound, single-file** — each order costs ~4 pump
   stages × a dozen ~3–4 ms NATS KV ops, and the gateway pump fans out to the
   four services sequentially. Levers, in order: parallelize the fan-out,
   raise the poll batch (32), pump services from independent loops,
   `spec.replicas` on ordering. None exercised — 1.8 orders/s ≈ 155k
   orders/day on a laptop was judged enough for the demo.

## How easy was it? (component-DX evaluation)

**The whole recreation is ~2 100 lines of new code** (1 525 Rust across 5
services, 144 WIT, 310-line single-file storefront, 120 lines of scripts) +
~400 lines of k8s yaml. It was built and verified (native lane + k8s, smoke
green on both) in one session. eShopOnDapr proper is ~10× that in C# before
counting Dapr component yaml, IdentityServer config, and Envoy config.

### What carried the weight (easy)

- **Coverage**: every Dapr building block had a sitting contract —
  `records:store` (state), `event:bus` (pub/sub), `fsm:workflow` +
  pump sweep (actors/reminders), `auth-guard`+`accounts-app` (IdentityServer,
  **zero new lines** — the identity service is two existing components
  composed), wasi:config (config/secrets). Nothing bespoke below the domain
  layer; `money`, `validate`, `paginate` weren't even needed (integer cents +
  three ifs sufficed — breadth exceeding need is the right failure mode).
- **The domain crates are pure routing + JSON glue.** The helpdesk-domain
  pattern (route match → introspect → records/fsm/bus call → JSON) transfers
  verbatim; payment is 91 lines.
- **One artifact, three hosts**: the same composed wasm ran under the native
  host and on k8s byte-identically — the entire choreography was debugged
  locally first, and the k8s smoke is the same script with one env var.
- **FSM-as-lifecycle beats actors for this**: eShop's order actor became nine
  declarative transitions; illegal event = 409 with the current state, history
  free.

### What was missing / cost real time (in descending order of pain)

1. **Push delivery.** `event:bus` is pull-only, so Dapr's push subscriptions
   became `/internal/pump` endpoints + a curl-loop Deployment + the SPA
   heartbeat. It works, but it's the one place the recreation feels hand-
   cranked, and it caps choreography throughput (see above). Gap: a
   `wasmcloud:messaging`-backed push variant of `event:bus`, or a host-level
   scheduler that invokes a component export on a timer.
2. **The atomics representation trap** (now fixed in event-bus): wasi:keyvalue
   doesn't pin how `atomics.increment` stores its counter — wash-runtime's
   NATS plugin writes 8-byte big-endian, the native host writes decimal
   strings. Reading a counter back via `store.get` silently returned "empty
   bus" on k8s only. Cost the single longest debug loop of the build. Lesson
   for every component: never read an atomics key through `store.get` without
   accepting both encodings.
3. **Fail-closed egress surprise**: wasi:http outgoing on v2 is deny-all until
   `localResources.allowedHosts` is set — correct design, but discovered via
   runtime WARN logs, not at deploy admission.
4. **No gateway/proxy capability.** The Envoy role was ~200 hand-written lines,
   half of it wasi:http outgoing boilerplate (build request → write body →
   subscribe → block → drain). **RESOLVED** (follow-up commit): extracted as
   `components/proxy-route` (`proxy:route@0.1.0`) — config route table
   (`routes` key, longest-prefix, trailing-`/` strips the prefix) + the whole
   outgoing round trip behind one `forward()` call. The gateway is glue again
   and the capability is reusable by any future edge component.
5. **Consumer boilerplate**: every consuming service repeats the same
   poll → deserialize → handle → ack loop per topic. A `bus:subscriber` helper
   contract (topic+group table → one callback export) would collapse ~40 lines
   per service and standardize the ack-on-failure semantics (today each service
   decides; ordering acks even when the FSM says no, which is right for dupes
   but easy to get wrong).
6. **Storage isolation is by convention** — one shared KV bucket, per-service
   record collections. Real per-service buckets need per-workload kv
   hostInterface config that the v2 CRD hints at but the build didn't exercise.

### Verdict

The capability library held: **6 services, 5 thin domain crates, 0 new
infrastructure components** — and both deploy-time bugs it surfaced
(atomics encoding, missing idempotency on a consumer) are contract-level
lessons, not code rot. The pull-based bus is the one architectural seam where
the Dapr original is genuinely more ergonomic; everything else was equal or
less code here.
