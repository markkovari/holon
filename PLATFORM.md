# Platform plan — a wasm-first multi-tenant PaaS (with a docker lane)

> **Correction (2026-08): the bet below was falsified on wasmCloud and has since
> been won.** Owning the host moved isolation into the linker
> ([ADR-0023](docs/adr/0023-isolation-is-a-linker-boundary.md)), so a guest string is
> a key into host-side state rather than a bucket selector — the exact failure
> ADR-0012 measured. Multi-tenant density is back and measured
> ([ADR-0033](docs/adr/0033-two-orgs-under-load.md),
> [ADR-0034](docs/adr/0034-two-machines-one-fleet.md)). The retreat described below —
> one host per application — is no longer what runs. Read
> [`docs/CURRENT.md`](docs/CURRENT.md) for the platform as it stands; this document
> is kept for the reasoning, not the conclusions.
>
> **Read [`docs/WHY.md`](docs/WHY.md) for the argument.**
>
> The isolation model below rests on shared hostgroups with per-tenant keyvalue
> buckets — "the density economics". That does not work: the bucket is chosen by the
> guest, not by manifest config, and two tenants were measured reading each other's
> records ([ADR-0012](docs/adr/0012-keyvalue-isolation-needs-a-cooperative-component.md)).
> Each application now owns a host with a private data bus
> ([ADR-0014](docs/adr/0014-an-application-owns-a-host.md)), which costs the
> multi-tenant density this plan was priced on.
>
> What survives, measured: **2.3 Mi per extra component inside a host against 70 Mi
> for a component in its own pod, and 1.2 ms saved per network hop avoided**
> ([ADR-0019](docs/adr/0019-the-density-number.md)). So the value is decomposing one
> app into many components — not packing many tenants onto one host. A
> single-component app should be a container.
>
> The forks in this plan are decided in [`docs/adr/`](docs/adr/); the ADRs win where
> they disagree, and several sections below (isolation model, phase 2's density
> assumption, the `buckets:` allow-list) are superseded rather than pending. The
> [index](docs/adr/README.md) has the current state and the open risks.

The product: an open service where tenants deploy **signed wasm components**
(and, secondarily, plain docker images) onto a shared wasmCloud v2 lattice on
k8s. Tenants compose apps from a **catalog** of components — private or
public — configured through `wasi:config` knobs only. State lives in the
platform's NATS JetStream KV or in the tenant's own external databases,
reached through per-tenant tunnels. First-class workload: **apps and AI
harnesses** (LLM providers behind `llm:inference`, SSE streaming, token
metering).

Everything below is grounded in measurements from `bench/HOST-PERF.md`
(rounds 1–6): the density bet (1600 instances / 10 pods @ 5% idle), the
per-node throughput (~1–2k rps app-path, 34k compute ceiling), the hybrid
brain/muscle topology (proven, round 6b), and SSE streaming (proven, 6d).

## What already exists (the unfair advantage)

| asset | role in the platform |
|---|---|
| 38 reusable capability components (`components/CATALOG.md`) | launch content: identity, records, rate-limit, quota, flags, TOTP, webhooks, fsm, cron, LLM interface |
| `auth-guard` | both a catalog item AND the platform's own tenant identity (it's already multi-tenant) |
| `tools/gen-catalog.py` + `catalog.json` | catalog service embryo (WIT + config-schema extraction) |
| tuned runtime deployment | helm values with pooling env, NATS sizing, `-DV` fix; two clusters running it |
| hybrid topology proof | k8s brain + bare-metal `wash host` muscle works today (round 6b) |
| bench harness (`bench-suite`, oha recipes) | becomes the SLO/regression suite |

## Architecture (the pieces, ~8 boxes)

```
                 ┌────────────────────────────────────────────┐
   tenants ───▶  │ 1. Platform API  (itself a wasm app)       │
                 │    tenants/projects/apps → workload specs  │
                 └───────┬────────────────────────┬───────────┘
                         ▼                        ▼
        ┌────────────────────────┐   ┌─────────────────────────┐
        │ 2. Registry + Catalog  │   │ 3. Admission            │
        │  OCI (wasm+docker),    │   │  cosign verify,         │
        │  WIT+config indexer,   │   │  publisher policy,      │
        │  public/private scopes │   │  isolation-kit injection│
        └────────────────────────┘   └────────────┬────────────┘
                                                  ▼
   ┌──────────────────────────────────────────────────────────┐
   │ 4. Runtime: k8s + runtime-operator + shared hostgroups   │
   │    (wasm lattice; docker lane = plain Deployments)       │
   │ 5. Ingress: Envoy/Traefik + ACME, Host-header → wasm     │
   │    hosts or docker Services                              │
   │ 6. State: 3-node JetStream KV; 7. Egress: per-tenant     │
   │    tailscale/WG proxies to external DBs                  │
   │ 8. Metering: usage events → aggregation → billing export │
   └──────────────────────────────────────────────────────────┘
```

Only 1, 2(indexer), 3, 8 and the isolation kit are net-new builds. 4–7 are
assembly of things already operated in this repo.

## Isolation model (the core design decision)

Shared hostgroups, tenants coexist in one lattice (the density economics).
wasm sandboxing gives compute isolation for free; the platform must add
**data and network isolation per workload**, all via mechanisms already
exercised here:

- KV/blob: per-tenant bucket names + the `buckets:` hostInterface allow-list
  (used in round 4 for blobstore) — the admission layer stamps these onto
  every workload spec, tenants never write raw hostInterface config.
- Egress: per-workload outbound allow-lists (host `allowed-host` mechanism);
  default-deny, tenant requests domains.
- Resources: `poolSize`/`maxInvocations` per workload + `WASMTIME_POOLING_*`
  budgets per host (round 5). Known gap: no hard per-workload CPU cap (fuel
  traps composed apps — round 6b finding); meter by invocations + wall-time
  until upstream improves.
- NATS: JetStream accounts per tenant (later; start with bucket prefixing).
- Docker lane: shared-kernel initially = "trusted images only" policy;
  microVM (Kata/Firecracker) is the gate for fully-untrusted containers —
  explicitly deferred to phase 4's decision point.

Known upstream constraint: a v2 host serves ONE environment, so per-tenant
environments would mean per-tenant hostgroups (rejected — that's the cost
model we're avoiding). Isolation is therefore workload-level until upstream
multi-backend hostInterfaces (#5051) lands, which would let one host serve
per-tenant KV backends natively. Track it.

## Phases (each ends shippable)

### Phase 0 — done
Capability library, tuned two-cluster deployment, catalog generator, SSE
proof, hybrid topology proof, bench harness.

### Phase 1 — single-tenant PaaS core (you are the tenant)
1. **App spec + deploy CLI**: `platform.toml` (components by catalog ref +
   config, domains) → generated WorkloadDeployment; `plat deploy|status|logs`.
   Start as a CLI translating specs → kubectl apply; no server yet.
2. **Registry with signing**: registry:2 + cosign sign-on-push, verify script.
3. **Catalog service v1**: gen-catalog reads from the *registry* (pull →
   `wasm-tools component wit`) instead of the source tree; serves
   catalog.json over HTTP (a wasm component, naturally).
4. **Ingress + ACME**: Traefik in front of both clusters' vet-clinic as the
   guinea pig; custom domain + TLS.
   **Exit test**: deploy vet-clinic end-to-end through `plat deploy` from a
   signed registry artifact on a clean namespace.

### Phase 2 — multi-tenancy
5. **Tenant model**: platform API as a wasm app (auth-guard + record-store +
   audit-log); orgs/projects/tokens.
6. **Isolation kit in admission**: webhook (or API-side generation) stamping
   bucket allow-lists, egress allow-lists, resource caps; name prefixing.
7. **Quotas**: per-tenant workload counts/invocation budgets via `quota`.
8. **Catalog scopes**: private/public publishing, publisher signing policy.
   **Exit test (adversarial)**: two tenants on one hostgroup; tenant A's
   component provably cannot read B's buckets, call B's services, or exceed
   its egress allow-list. This test is the product.

### Phase 3 — metering + AI harness kit
9. **Usage collector**: host telemetry → JetStream usage events →
   aggregation in record-store; per-tenant daily rollups.
10. **Token metering middleware**: a component exporting `llm:inference` and
    importing `llm:inference` (wrap any provider), counting tokens per
    tenant/model → usage events. ~300 lines on top of `quota`.
11. **Provider set**: openai-provider exists; add anthropic-provider; mock
    for CI. SSE template app (proven path, round 6d).
    **Exit test**: a deployed AI harness shows a per-tenant token bill.

### Phase 4 — docker lane + external state
12. **BYO-image deployments** behind the same ingress + spec format
    (`kind = container`); trusted-images policy documented.
13. **Egress connect**: per-tenant tailscale/WG proxy + DNS injection;
    example: component using `wasi:sockets` → tenant's managed Postgres.
14. **MicroVM decision point**: only if untrusted containers become a real
    demand; otherwise stays a documented non-goal.

### Phase 5 — scale + hardening
15. **Brain/muscle expansion**: rented metal hosts joining over NATS (round
    6b recipe: certs + `--add-host` + pooling env), JetStream leaf nodes on
    muscle for KV locality (the 98-vs-1266 rps lesson).
16. **SLO suite**: bench harness as scheduled regression runs; publish the
    ladder per release.
17. **Upstream debts**: file/track — chart `-DV` + 128Mi NATS defaults,
    engine flags (#5023), ghost Host CR scheduling, fuel-vs-composed-apps,
    concurrent blob write wedge, multi-backend hostInterfaces (#5051).

## Risks, honestly

- **wasi:keyvalue has no CAS** → all RMW state is single-writer/best-effort;
  documented consistency envelope per catalog component; `lock-mutex` is
  advisory-only. Mitigation: JetStream-native CAS plugin later.
- **NATS is control AND data plane** → 3-node JetStream before any real
  tenant; its sizing is the platform's availability story (round 5 proved
  what defaults do).
- **v2 API surface is young** (rc-grade CRDs, scheduler quirks we've already
  hit) → pin versions, keep the bench suite as the canary, expect churn.
- **Per-workload CPU isolation is weak today** → noisy-neighbor risk on
  shared hosts; mitigate with maxInvocations + host-level budgets + metering
  alerts, and keep a "dedicated hostgroup" tier as the escape hatch (it's
  also the premium SKU).
- **Streaming under concurrency** unmeasured (6d was single-stream) — add a
  c50 SSE soak to the bench suite in phase 1.

## Build order, first 10 tasks (phase 1 granularity)

1. `platform.toml` spec + `plat` CLI skeleton (spec → WorkloadDeployment).
2. cosign sign/verify wrapper for `wkg oci push` artifacts.
3. Catalog indexer over the registry API (extend gen-catalog).
4. Catalog HTTP service (wasm component serving catalog.json).
5. Traefik + ACME in front of picur's cluster; vet-clinic on a real domain.
6. `plat deploy` end-to-end demo (exit test).
7. SSE soak endpoint at c50 (bench suite).
8. Isolation-kit generator (bucket/egress stamping) as a pure function +
   tests — before the tenant model, so phase 2 starts adversarially testable.
9. anthropic-provider component (mirror openai-provider).
10. Usage-event schema + collector stub writing to JetStream.
