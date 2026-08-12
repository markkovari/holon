# The platform as it stands

What runs today, what is measured, and what is honestly missing. The reasoning lives
in [77 ADRs](adr/); this page is the map.

Last revised after ADR-0077.

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
| a secret key | `SecretRef`, then a value it never sees | [0051](adr/0051-the-secret-reader.md), [0061](adr/0061-the-secret-reader-was-never-linked.md) |

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
| one cross-node call | **57 µs** (5.4% of a do-nothing request) ([0074](adr/0074-the-split-graph-still-works.md)) |
| losing the STORE server at `--kv-replicas 3` | 0 errors, state intact, leader re-elected ([0067](adr/0067-one-copy-is-not-a-backup.md)) |
| losing a whole MACHINE's store, 3 real machines | 0 errors, counter unbroken; the host failed over ([FLEET-BENCH](../bench/FLEET-BENCH.md)) |
| the tailnet's own cost, request touching no storage | 41 707 rps loopback vs 1 230 over Tailscale ([FLEET-BENCH](../bench/FLEET-BENCH.md)) |
| a rate limit stored as keyed state, not a record | 85 store ops per request → **2**, 4.8× ([0070](adr/0070-a-rate-limit-is-not-a-record.md)) |
| a real app's store mix, under load | 99.6% reads, **264 reads per write** ([0062](adr/0062-what-a-real-application-asks-the-store-for.md)) |
| reads a perfect cache would serve | 99.8%, working set 1 926 keys ([0062](adr/0062-what-a-real-application-asks-the-store-for.md)) |
| durable reads with `--kv-cache-ms 1000` | 99.7% served; NATS reaches the in-memory numbers ([0063](adr/0063-a-ttl-is-cheaper-than-coherence.md)) |
| the slowest route, and why | login at 214 rps — argon2, unchanged by any backend or cache ([0063](adr/0063-a-ttl-is-cheaper-than-coherence.md)) |

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

173 across four crates, `cargo nextest`. No Python anywhere in `bench/` or `e2e/`.

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
| `reconciler/tests/reveal.rs` | a guest reveals the key it was granted, and only that one |
| `reconciler/tests/staleness.rs` | cross-node staleness, and the lost update it causes |
| `reconciler/tests/ha.rs` | two ingresses, then one dies |
| `reconciler/tests/leader.rs` | two reconcilers: one acts, and the standby takes over |
| `reconciler/tests/publish.rs` | public needs a real signature over the real digest |
| `reconciler/tests/crossnode.rs` | one graph over two nodes, both links over wrpc |
| `bench/` | only what drives *other machines* — malna, bobocat, a k8s wasmCloud |

## Honestly missing

- **No `@version` in a catalogue key — and this is now a FEATURE request, not a
  gap** (ADR-0076). ADR-0007 rule 1 is held by binding `public` to the signed
  digest, and revocation turned out to need provenance rather than versions. What
  is missing is several live versions of one component (rollback, a beta beside a
  stable), and the key is also the blob key, the push-queue key and the deployment
  handle, so it is a migration through the deployment path.
- **No in-transit wrapping** on the secret fetch — TLS only. Replay is closed
  (ADR-0071: a nonce claimed exactly once, inside a 60s window), but an attacker
  who can read the transport still reads the plaintext. Nothing sweeps spent
  nonces yet; they are keyed by window so a sweeper can drop one by prefix.
- **No UI.** `POST /api/components/satisfies` answers "would this plug fit" with wac's
  real subtype check, and nothing calls it: a facility, not yet a feature.
- **The loop does not shard**, on purpose (ADR-0072). It now elects a leader, so
  a standby takes over within the lease TTL plus one interval — the reconciler was
  the only control component without one. Sharding stays unbuilt: the pass after a
  fleet change is 1.23 s at 1000 nodes × 10 000 apps, which is 12% of one 10 s
  interval. The number to watch is `comp-planscale`'s cold column against
  `--interval`.
- **The read cache is off by default because reads go stale, not because writes
  are lost.** ADR-0065 measured a lost update — `record-store::update` enforced its
  revision guard as a read-compare-write over the very `wasi:keyvalue` the cache
  sits under — and ADR-0066 fixed it by moving the comparison into the store
  (`comp:store/cas`, JetStream's own revision on NATS). What remains is the
  documented trade from ADR-0064: a plain read can be up to the TTL stale, so
  read-your-own-writes does not hold across nodes. That is a semantic to opt into.
- **Nothing SCHEDULES the index check.** A record and its indexes are separate
  writes, so a crash between them leaves them disagreeing. A read now reports the
  half it can see (`{"drift":true,…}` from `list`), and `verify` reports both
  halves without fixing anything (ADR-0075) — but it is a question nobody is
  asking on a timer. A cron or a pass folded into the reconciler closes it.
- **Drift lines land in the tenant's host log**, since `record-store` runs in the
  tenant's graph, and nothing aggregates those yet (ADR-0075).
- **`list-keys` returns keys as STORED on the NATS backend**, not as the guest
  wrote them. For every component here that is identical, because they sanitize
  their own key segments; a key containing bytes that needed escaping comes back
  escaped. Making it reversible renames every key already written (ADR-0068).
  wasmCloud's provider does no encoding at all and lets NATS reject what it will
  not take — a different trade, checked rather than assumed (ADR-0069).
- **Conduit's `feed` still does one favorites lookup per article.** The author and
  follow lookups are gone (ADR-0077: 12 fewer store reads per request, 35% fewer
  over a run), but favorites genuinely differ per article, so removing them needs
  either a `find-by` over many values or a denormalised counter — a second source
  of truth, which is the class of bug this repo keeps removing.
- **Cross-machine benchmarks now have one real run** (`bench/FLEET-BENCH.md`:
  three Macs, R3, a machine killed under load). What is still unproven is the
  malna/bobocat *scripts* — they target a Linux aarch64 Pi build and were not
  exercised by that round. What has been checked without them: every
  `comp-bench` subcommand and flag the scripts pass still exists, as does every
  flag they pass to `comp-host`, `comp-stub`, `comp-reconciler` and `comp-ingress`;
  and the local `bench/tenancy/run.sh` runs clean end to end (3 nodes, both orgs on
  every node, ~4.8k rps each). Three Justfile recipes — `shared-state`,
  `five-nodes`, `split-graph` — called scripts deleted three commits ago and are
  gone. The remote scripts now **fail immediately** when a machine is missing
  (`bench/preflight.sh`): before, `ssh -f -n` failed silently and the run printed a
  number for a fleet that never spanned two machines.
- **The hop's PERCENTAGE is not a platform property.** ADR-0074 re-measured a
  cross-node call at **57 µs** — 5.4% of a request that deliberately does almost
  nothing, and a far smaller share of one that touches JetStream. Quote the
  microseconds, not the percentage.
