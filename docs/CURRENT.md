# The platform as it stands

What runs today, what is measured, and what is honestly missing. The reasoning lives
in [80 ADRs](adr/); this page is the map.

Last revised after ADR-0083.

## Shape

Bare metal joined by NATS — no Kubernetes anywhere on the runtime path.

```
holon (CLI) ─┐
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
| `holon` | the CLI |

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

See [SCENARIOS.md](SCENARIOS.md) for what a graph run can and cannot do today, at
three levels of difficulty, with every claim marked covered / runnable / blocked.
Its summary in one line: everything that CARRIES work is built and has been
broken on purpose to prove it; everything that DECIDES is designed and unbuilt.

## Two rules that removed whole classes of failure

Both were learned the same way — the same bug four times, dismissed as flakiness
three of them — and both are now enforced by a helper rather than by remembering.

- **Do not have a separate readiness signal.** Retry the operation the test
  actually cares about (`Fleet::until`). Every readiness probe chosen separately
  from the thing being measured has eventually proved something adjacent to it: a
  root route that touches no capability, an ingress `serves()` satisfied through
  activation with an empty routing table, a call refused before any HTTP happens.
  Each passed alone and failed under load.
- **A dropped connection is not an answer.** Retry transport failures, never
  results. A stale read and a rejected candidate arrive as perfectly good
  responses, and retrying THOSE retries away the phenomenon under test until it
  happens to look right — green and worthless.

## What the loop does now

Everything in this section is built, and most of it carries a number, because a
claim about a distributed system without one is a hope.

- **A branch runs, and a generation compares branches.** `graph:run/driver` joins
  the writer to the gate — attempt, judge, repair from what the checks actually
  said, stop for a stated reason (`accepted`, `plateau`, `exhausted`,
  `no-progress`, `over-budget`) — and keeps the best candidate by score rather than
  the last, because a repair can be worse than what it repaired. The budget is in
  TOKENS, reported with each candidate; it under-reports by whatever the unusable
  answers cost, which is why `max-attempts` remains the hard bound.
- **`graph:select/selector` is the only path from a branch to a pull request**, and
  the gate is enforced by the SHAPE of `land`, which takes runs rather than files:
  there is no argument by which a caller lands a candidate the checks rejected.
  Every selection reports how many DISTINCT candidates a generation produced,
  because a herded generation looks exactly like a healthy one.
- **`generation::fan_out` is native**, because a component runs one call at a time
  and a generation whose branches ran in sequence would be a for-loop wearing the
  word. Measured at 4 branches: **1948 ms wall against 5849 ms of branch time**.
- **`generation::search` runs generations of generations**, each seeded with the
  last one's best candidate AND the checks it still failed. Proven against a goal
  no single generation can reach. Diversity is authored: each branch gets a lens,
  and exactly one branch per generation is shown nothing from the previous one.
- **`generation::compose_search` runs a DECOMPOSED goal** — K parts that compose
  rather than N branches that compete, each part its own generation, a contract
  they agree through, and one pull request (ADR-0086). Run against this repository:
  both halves green, the join gate passed, PR opened.
- **A composition is derived, not written.** `bin/compose <component>` reads the
  built artifact's imports, finds the components exporting those interfaces, and
  plugs them — recursively, because a capability has capabilities of its own, and
  content-addressed, so the result is built once and outlives the run. This closes
  the gap that made a goal-built component undeployable: 59 hand-written `wac plug`
  chains live in the `Justfile`, and until now anything the loop produced needed a
  human to add the sixtieth. The derived composition is also strictly more complete
  than the hand-written one — `just compose-vet` leaves 16 capabilities dangling
  (`ai:inference`, `blob:store`, `money:amount`, `otp:totp`, …) that `bin/compose
  vet-domain` binds. Two things it learned the hard way and now encodes: a FLAT
  plug chain silently hoists each plug's own imports into the result (which is why
  the `Justfile` pre-composes `auth-guard`), and interface granularity matters —
  `cache-backing` exports `cache:store/sink` and `/source` but not `/cache`, so a
  package-level match reports "satisfied" for an import that then dangles.
- **The gate is real and joined.** `comp-checks` materialises a candidate over a
  base tree, runs allow-listed commands, and reports the check vector; it is native
  because a component cannot spawn a process, which is the sandbox working rather
  than a gap. The runner needs no checkout — the base tree is posted once, keyed by
  its commit id, and later candidates send only a diff. An unknown commit is ASKED
  for (409) rather than substituted.
- **The swarm has a memory with a policy** (ADR-0084). `knowledge:memory` decides
  who may write what the swarm believes — an agent's world has `memory` and not
  `promotion` — retrieves by fusing SurrealDB's KNN with `search:index`'s TF-IDF,
  and weights what it returns by what happened to the runs that read it. `holon
  goal run --surreal-url …` asks whether a goal has already been done before
  spending a generation, and records every branch's verdict on the way out.
- **Two parts can negotiate an interface** (ADR-0086). `contract:registry` versions
  it; a part asks, another grants, denies or counters; an amendment is canonical
  only once the granting part's own gate passes against it; and nothing blocks
  inside a generation, so a needed change costs a generation and never a deadlock.
- **Desired state pages.** It was silently truncated at 1000 records — a stress run
  that grew 3906 environments flatlined at exactly 500 running, every one past the
  cap accepted, reported created, never placed. Whole-collection reads now page and
  the backstop reports when it is reached: **781/781 converges** where 500 was the
  ceiling.
- **Admission control is enforced.** 429 above `max-placement-lag`, 503 when the
  reconciler's report goes stale — fail closed, because a dead loop is exactly when
  accepting more is pointless. It counts what it has let through since the last
  report, so a burst cannot outrun it: **625 spawns in 0.2 s cut to 435**, and all
  591 resulting apps converge.
- **Component bytes are staged by CONTENT**, so two workers pushing different
  builds cannot lose each other's; re-uploading identical bytes is a no-op. The WIT
  surface is a compatibility gate and never an identity check: a save refuses to
  remove an export (`?force=true` when that is the intent), but only content
  decides what an artifact IS.
- **The loop elects a leader**, so a standby takes over within the lease TTL plus
  one interval. It does not shard, on purpose (ADR-0072): the pass after a fleet
  change is **1.23 s at 1000 nodes × 10 000 apps**, 12% of one 10 s interval.
- **`just test` compiles every test target first, then runs them** — 356 tests, a
  number nobody could state before, because a workspace whose tests had never
  compiled hid 34 of them and no suite complained.

## Known behaviours, not gaps

Things that surprise people, are deliberate, and are cheaper to read here than to
rediscover.

- **A successful request does not mean the fleet has converged.** An ingress with
  an empty routing table still answers: it asks the reconciler to activate the app
  and routes to whatever comes back. So "poll until requests stop failing" goes
  green while inventory is empty. `Fleet::wait_for_placement` reads inventory,
  which is what routing is actually built from. This cost four wrong diagnoses.
- **The read cache is off by default because reads go STALE, not because writes are
  lost.** The lost update ADR-0065 measured is fixed in the store (`comp:store/cas`).
  What remains is ADR-0064's documented trade: a plain read can be up to the TTL
  stale, so read-your-own-writes does not hold across nodes. Opt into it knowingly.
- **`list-keys` returns keys as STORED on the NATS backend**, not as the guest
  wrote them. Identical for every component here, which sanitises its own segments;
  making it reversible would rename every key already written (ADR-0068).
- **The hop's PERCENTAGE is not a platform property.** A cross-node call is **57 µs**
  (ADR-0074) — 5.4% of a request that deliberately does almost nothing, and far less
  of one that touches JetStream. Quote the microseconds.
- **`cargo component check` and `cargo component build` are not gates.** Both
  succeed on a crate that implements none of its world — measured twice, while
  running a real goal. Any goal whose checks are those commands is gated on nothing.

## Honestly missing

One line each, and where the work lives. Anything with a goal number is on the
worklist in [`.comp/goals/`](../.comp/goals/).

**The loop's judgement**

- **The pool is a closed loop now**: a failed branch writes what it failed on, each
  branch reads a different slice, a passing candidate is distilled into `patterns`
  by the one interface an agent cannot reach, what a branch read is judged by what
  happened to it, and every run forgets what nobody has read. Both e2es prove it
  without an AI call. What is NOT proven is that any of it makes a real model
  better — that needs a goal a real model fails, and the runs so far have not
  found one.

- ~~**Nothing criticises a gate.**~~ → **built**: every check, the goal's and each
  part's, is run against the untouched base before anything is spent, and a run is
  refused when one of them passes. What it does NOT check is whether a gate
  measures the right thing — the empty-corpus candidate that passed everything
  would still pass everything. → goal 07
- **The loop writes and reads; it does not yet promote or forget.** A failed branch
  writes what it failed on in the gate's own words — no model in that path, so
  negative knowledge cannot be a hallucination. Each branch of an ordinary run
  reads lessons — a different `k` and a different pool mix per branch, the
  control arm reading nothing — and what a branch read is attributed to its verdict
  when the run ends, so a lesson present when runs fail sinks. A candidate that passes is
  distilled into at most 900 characters and promoted to `patterns` by the one
  interface an agent cannot reach. Every run sweeps the pool on
  its way out, so it stays bounded without a daemon, and a decomposed run's PARTS
  do all of it too — each on its own goal. → goal 08, ADR-0084
- **Nothing measures herding or churn.** The diversity knobs exist (a lens per
  branch, one branch that reads nothing); no run reports that its generation
  converged. A negotiation was observed climbing v3 → v7 while no score moved, and
  nothing noticed. → goal 03, ADR-0086
- **A branch cannot differ by MODEL.** An environment is a copy of its parent, so a
  generation cannot put haiku on three branches and opus on the fourth. → goal 03
- **No structural memory of an app that already exists.** ADR-0085 designs a derived
  code graph and answers none of its isolation question: where a shared index lives,
  when a store is named after its app. → ADR-0085

**The platform**

- **Nothing notices an inventory TTL mismatch.** Three processes declare a TTL on
  one shared bucket and whoever creates it first wins, silently; they agree today
  only because three defaults coincide at 15 s. The bucket's real `max_age` is never
  compared with the one asked for.
- **Convergence under load is unmeasured.** Placement can lag past ten seconds and
  nobody has measured it as a function of load; the reconcile interval is the
  obvious suspect.
- **Pinning is understood and not acted on.** `shop`, `shop:v2`,
  `shop@sha256:<hex>` parse and are tested; a deployment still names a bare id.
- **Admission does not cover component pushes**, and its limit is a flat number
  rather than anything derived from fleet size.
- **Breadth is unmeasured beyond 8** branches (~3 s on one node). Depth is measured
  to 4.
- **Nothing schedules the index check.** A record and its indexes are separate
  writes; a read reports the half it can see and `verify` reports both (ADR-0075),
  but nothing asks on a timer. Nothing aggregates the drift lines either — they land
  in the tenant's host log.
- **No `@version` in a catalogue key** — a feature request rather than a gap
  (ADR-0076): several live versions of one component, for rollback or a beta beside
  a stable. The key is also the blob key, the push-queue key and the deployment
  handle, so it is a migration through the deployment path.
- **No in-transit wrapping** on the secret fetch — TLS only. Replay is closed
  (ADR-0071), but a reader of the transport reads the plaintext, and nothing sweeps
  spent nonces.
- **The database is not part of the platform.** SurrealDB is an external service on
  an egress allow-list: nothing deploys it, backs it up, replicates it, or notices
  when it is gone — all of which the KV path already does.
- **No UI**, and `POST /api/components/satisfies` (wac's real subtype check) is a
  facility nothing calls.
- **No automated cover for the interactive secret prompt** — the pipe and `--from`
  paths are tested, the terminal path was verified by hand and needs a pty.
- **Conduit's `feed` still does one favourites lookup per article.** The author and
  follow lookups are gone (ADR-0077: 12 fewer store reads per request, 35% fewer
  over a run); favourites genuinely differ per article, so removing them needs a
  `find-by` over many values or a denormalised counter — a second source of truth.
- **The remote bench scripts are unproven.** One real cross-machine run exists
  (`bench/FLEET-BENCH.md`: three Macs, a machine killed under load); the
  malna/bobocat scripts target a Linux aarch64 build that round never exercised.
  They now fail immediately when a machine is missing rather than printing a number
  for a fleet that never spanned two.
