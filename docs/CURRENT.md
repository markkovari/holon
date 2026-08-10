# The platform as it stands

What runs today, what is measured, and what is honestly missing. The reasoning lives
in [51 ADRs](adr/); this page is the map.

Last revised after ADR-0059.

## Shape

Bare metal joined by NATS — no Kubernetes anywhere on the runtime path.

```
comp (CLI) ─┐
            ├─→ platform-domain ── a wasm component, itself hosted by comp-host
browser ────┘   (orgs, catalogue, market, secrets, deployments, revisions)
                        │  HTTP
                comp-reconciler ── diffs desired vs observed, sends commands
                        │  NATS  comp.v1.<lattice>.cmd.<node>.<verb>
        ┌───────────────┼───────────────┐
    comp-host       comp-host       comp-host      ← one process per NODE,
    (every tenant)  (every tenant)  (every tenant)    every tenant inside it
        └───────────────┴───────────────┘
                        ▲  routes by Host header
                  comp-ingress ── balances, sheds, activates
```

| binary | what it is |
|---|---|
| `comp-host` | the runtime. wasmtime 45, one process per node, every tenant inside it |
| `comp-reconciler` | the loop. A pure `plan()` diff plus command dispatch |
| `comp-ingress` | the door. Host-header routing, least-outstanding, shedding, activation |
| `comp-stub` | a stand-in control plane for tests and benchmarks |
| `comp-bench` | reads benchmark output; the only thing that interprets a number |
| `comp-planscale` | times `plan()` over synthetic fleets — control-loop scaling |
| `comp` | the CLI |

## The one rule everything else is an application of

> A name is a real boundary iff **(1)** it is chosen by host-side state the guest
> cannot write, and **(2)** the guest has no second path into the namespace.

Applied four times, each enforced by a private newtype rather than by review:

| what the guest names | what the host resolves it to | ADR |
|---|---|---|
| a store name (`"default"`) | `BucketId` | [0023](adr/0023-isolation-is-a-linker-boundary.md) |
| an import interface | `InstanceId` in the link table | 0013 |
| a config key | a value the uploader declared | [0047](adr/0047-config-is-declared-and-checked.md) |
| a secret key | `SecretRef`, then a value it never sees | [0051](adr/0051-the-secret-reader.md) |

## What is measured

Every number below is from a run recorded in an ADR, not an estimate.

| | |
|---|---|
| cross-tenant reads, adversarial sweep | **0** ([0023](adr/0023-isolation-is-a-linker-boundary.md)) |
| two orgs on one fleet, concurrent | 3 313 rps each, p99 17 ms, 100% `200` ([0036](adr/0036-open-loop-stress-and-a-correction.md)) |
| every node holding more than one org | yes, 4/4 ([0034](adr/0034-two-machines-one-fleet.md)) |
| node RSS, idle / holding apps | 12 MiB / 52 MiB ([0034](adr/0034-two-machines-one-fleet.md)) |
| losing a machine under load | 0 requests failed, back to full replicas in 16–17 s ([0035](adr/0035-losing-a-machine.md)) |
| overload with shedding | p99 42 s → 747 ms, and *more* work served ([0041](adr/0041-the-ingress-sheds-load.md)) |
| start, cold / warm / shared | 35.2 ms / 0.43 ms / **0.08 ms** ([0040](adr/0040-compiled-artifacts-are-cached.md), [0052](adr/0052-one-copy-per-digest.md)) |
| idle app, marginal — shared digest / own digest | **~0.03 MiB** / ~2.0 MiB ([0053](adr/0053-the-matrix.md)) |
| 32 idle apps, one digest vs 32 | 48.4 MiB vs 112.4 MiB — 57% less ([0053](adr/0053-the-matrix.md)) |
| pooling allocator, now the default | **3.1×** with storage out of the way ([0057](adr/0057-the-latency-column-was-arithmetic.md)) |
| memory under 10 min of constant load | plateaus at 99 MiB, returns 23 MiB — no leak ([0054](adr/0054-pooling-on-and-the-leak-that-was-not.md)) |
| a reconciler pass, 1000 nodes / 10 000 apps | 1 227 ms cold, **46 ms** steady ([0056](adr/0056-a-converged-app-keeps-its-placement.md)) |
| one node, storage out of the way | **30 545 rps**, p50 1.55 ms, p99.9 3.56 ms ([0057](adr/0057-the-latency-column-was-arithmetic.md)) |
| the same, on the NATS store | 9 755 rps — every earlier rps was this ([0057](adr/0057-the-latency-column-was-arithmetic.md)) |
| the ingress hop | 21% ([0057](adr/0057-the-latency-column-was-arithmetic.md)) |
| inventory snapshot ceiling | ~50 000 instances per node, zstd'd ([0058](adr/0058-snapshots-compress-and-parses-are-reused.md)) |
| scale to zero and back | parked at 0, served in 49 ms, parked again in 5 s ([0042](adr/0042-scale-to-zero-and-back.md)) |
| vs wasmCloud 2.5.2, same component | 3.6× on the Mac, 2.3× on a Pi ([0039](adr/0039-comp-versus-wasmcloud.md)) |

## Authoring an app

`comp/v1` YAML. The platform stamps the digest, `host_needs`, `egress` and the tenant;
an author writes none of them ([`spec.rs`](../reconciler/src/spec.rs)).

```yaml
version: comp/v1
app: shop
strategy: linked          # or fused — wac-composed at build time
components:
  - id: gate
    scale: { min: 1, max: 4, target: 20 }   # concurrent requests per replica
    config: { grace-period-secs: "5" }
    secrets:
      - key: stripe
        ref: vault://acme/stripe            # by reference, never a value
links:
  - from: gate                              # consumes
    import: records:store/store@0.1.0
    to: record-store                        # provides
ingress:
  host: shop.acme.example.com
  component: gate
```

Refused at save, with the reason an author can act on: an unknown config key (and the
legal ones), a required key that is unset, a secret reference that does not resolve or
belongs to another org, two providers of one interface, a capability no host grants, a
constraint no node advertises.

## Operating

Tunables come from `comp.toml`, the environment, or flags — flag beats env beats file
beats default, and a misspelled key is an error ([`comp.example.toml`](../comp.example.toml)).

The knobs that matter: `settle_passes` (the scale-down cooldown), `inventory_ttl` (how
fast a dead machine is noticed), `max_inflight` (where the ingress starts shedding).

## Tests

158 across four crates, `cargo nextest`. No Python anywhere in `bench/` or `e2e/`.

```
cargo build --release --manifest-path host/Cargo.toml   # tests spawn this
cargo nextest run --release --manifest-path reconciler/Cargo.toml
```

| suite | what it holds |
|---|---|
| `reconciler/tests/e2e.rs` | six manifests on one fleet — three serve, three are refused for the right reason |
| `reconciler/tests/scaling.rs` | replicas follow demand; shedding grows the app |
| `reconciler/tests/state.rs` | two replicas share one count; node-local stores are refused |
| `reconciler/tests/coldstart.rs` | 35 ms vs 0.43 ms, and a corrupt cache recovers |
| `reconciler/tests/secrets.rs` | one org's secrets are invisible to another by every route |
| `reconciler/tests/ha.rs` | two ingresses, then one dies |
| `bench/` | only what drives *other machines* — malna, bobocat, a k8s wasmCloud |

## Honestly missing

- **`public` catalogue visibility is 501.** It needs signing (ADR-0025); an unsigned
  public catalogue is worse than none. Private and org work.
- **No `@version` in a catalogue key**, so visibility is per component rather than per
  version, which ADR-0007 says it should be.
- **The secret reader does not check references at start**, so a bad one surfaces at
  first `reveal` rather than at start — weaker than the fail-closed rule ADR-0051
  itself states.
- **No in-transit wrapping or replay protection** on the secret fetch — TLS only, and
  a captured request can be replayed until the token expires.
- **No UI.** `POST /api/components/satisfies` answers "would this plug fit" with wac's
  real subtype check, and nothing calls it: a facility, not yet a feature.
- **The loop does not shard.** One reconciler, no leader election. A steady pass
  at 1000 nodes × 10 000 apps is 46 ms, but the pass after any fleet change is
  1.23 s and that one is `apps × nodes` (ADR-0056).
- **Nothing caches keyvalue reads.** Each is a JetStream round trip and it is the
  largest cost on the request path; the obvious mirror was built, measured at
  2.3× slower on a write-heavy workload, and reverted (ADR-0059).
- **No real application has ever been profiled**, only a benchmark component
  chosen because it hammers storage — which is the least representative shape for
  every caching decision above.
- **Cross-machine benchmarks are unproven since the refactor.** The scripts were
  rewired to `comp-bench` and have not been run against malna or bobocat since.
