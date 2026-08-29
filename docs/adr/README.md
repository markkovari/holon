# Architecture decisions

Numbered, dated, one decision each, superseded rather than edited. Format and rules
in [ADR-0001](0001-use-adrs.md).

For the platform **as it stands** — what runs, what is measured, what is missing —
read [`../CURRENT.md`](../CURRENT.md). These are how it got there.

`docs/PLATFORM.md` is the original narrative plan, kept for its reasoning: its central bet
was falsified on wasmCloud and then won by owning the host (ADR-0023), so its
conclusions no longer describe what runs. Where anything disagrees, the ADR wins.

**[`../WHY.md`](../WHY.md) is the value proposition, with the measurements behind it.**
**Read [ADR-0019](0019-the-density-number.md) for the numbers themselves.** The
multi-tenant density bet docs/PLATFORM.md was built on is falsified (ADR-0012, ADR-0014). What
survives, measured: **2.3 Mi per extra component inside a host against 70 Mi for a
component in its own pod, and 1.2 ms saved per network hop avoided** — and under load,
**identical throughput and CPU, 3.2× less memory, and a 36% better p99**
([ADR-0020](0020-the-density-number-under-load.md)). So the value here is
decomposing one app into many components, not packing many tenants onto one host — and a
single-component app should be a container, not a wasm workload.

## Where to start, by what you are doing

Ninety-seven decisions is not a reading list. Four paths through it:

| you want to | read |
|---|---|
| understand the **isolation** claim | [0023](0023-isolation-is-a-linker-boundary.md) → [0015](0015-a-bucket-name-is-not-a-boundary.md) → [0047](0047-config-is-declared-and-checked.md) → [0051](0051-the-secret-reader.md), [0061](0061-the-secret-reader-was-never-linked.md) |
| **compose** components into an app | [0005](0005-deployment-strategy-is-a-tenant-choice.md) → [0048](0048-does-this-plug-fit.md) → [0087](0087-a-composition-is-derived-not-written.md) |
| **deliver** an app to machines | [0021](0021-there-is-no-kubernetes.md) → [0022](0022-desired-state-is-a-manifest.md) → [0024](0024-artifacts-are-content-addressed.md) → [0029](0029-one-address-in-front-of-n-replicas.md) → [0095](0095-what-is-allowed-to-be-native.md), [0096](0096-a-pull-contract-needs-a-relay.md) |
| know why the **agentic loop** is shaped as it is (paused) | [0078](0078-an-environment-is-a-derived-app.md) → [0086](0086-parts-negotiate-a-contract.md) → [0089](0089-capability-accumulation.md) → [0091](0091-one-store-one-schema.md) |

The last path is history rather than active work — the loop is
[paused](../../README.md#the-agentic-loop--paused-and-kept), and its ADRs are kept
under the same rule as every other superseded-but-instructive decision here: nothing
is deleted, because how it got here is the point.

**This table is what is in force.** Superseded decisions are kept — they are how it
got here — and listed separately at the end so that reading the list top to bottom
tells you what is true now rather than what was once believed.

| # | decision | status |
|---|---|---|
| [0001](0001-use-adrs.md) | Record architecture decisions as ADRs | accepted |
| [0005](0005-deployment-strategy-is-a-tenant-choice.md) | Deployment strategy is a tenant choice: fused or linked | accepted |
| [0006](0006-artifacts-are-digest-pinned-oci.md) | Artifacts are digest-pinned OCI; the WIT surface is the contract | accepted; durability + auth revised by [0017](0017-the-applier-pushes-and-the-registry-is-a-cache.md) |
| [0007](0007-component-visibility-and-sharing.md) | Component visibility: private, org, public — and what public costs | accepted |
| [0009](0009-identity-reuses-auth-guard.md) | Sign-in reuses `auth-guard`; OIDC is a later swap | accepted |
| [0010](0010-config-and-secrets.md) | Config is `wasi:config`; secrets never enter a manifest | accepted |
| [0012](0012-keyvalue-isolation-needs-a-cooperative-component.md) | Per-tenant keyvalue isolation needs a cooperative component | accepted; **answered** by [0023](0023-isolation-is-a-linker-boundary.md) |
| [0015](0015-a-bucket-name-is-not-a-boundary.md) | A bucket name is not a boundary, and `hostInterfaces[].name` does not work | generalised by [0023](0023-isolation-is-a-linker-boundary.md) |
| [0019](0019-the-density-number.md) | The density number, measured: 2.3 Mi per component, 70 Mi per app | accepted (idle/cold-start figures) |
| [0020](0020-the-density-number-under-load.md) | The same density number, under load: free throughput, 3.2× memory, better tail | accepted |
| [0021](0021-there-is-no-kubernetes.md) | There is no Kubernetes; nodes are a lattice you join | accepted |
| [0022](0022-desired-state-is-a-manifest.md) | Desired state is a manifest; the reconciler pulls it | accepted |
| [0023](0023-isolation-is-a-linker-boundary.md) | Isolation is a linker boundary, not a process boundary | accepted; measured in [0026](0026-the-adversarial-run.md); its backend table corrected by [0027](0027-a-spread-app-needs-a-shared-store.md) |
| [0024](0024-artifacts-are-content-addressed.md) | An artifact is its digest, and the object store is a cache | accepted |
| [0025](0025-slice-one-on-the-lattice.md) | Slice one, on the lattice: two boxes, one killed node | accepted; its cross-node reasoning corrected by [0028](0028-cross-node-calls-are-wrpc.md) |
| [0026](0026-the-adversarial-run.md) | The adversarial run: contained, at 10.5k rps, in 56 MiB | accepted; **discharges 0023's measurement** |
| [0027](0027-a-spread-app-needs-a-shared-store.md) | A spread app needs a shared store, and the platform now refuses otherwise | accepted |
| [0028](0028-cross-node-calls-are-wrpc.md) | Cross-node calls are wRPC; the codec I designed should never have existed | accepted |
| [0029](0029-one-address-in-front-of-n-replicas.md) | One address in front of N replicas, and its table comes from inventory | accepted; its balancer replaced by [0030](0030-least-outstanding.md) |
| [0030](0030-least-outstanding.md) | Least-outstanding, because round robin collapsed on a real fleet | accepted |
| [0031](0031-an-org-owns-a-deployment.md) | An organisation owns a deployment, and a person can be in several | accepted |
| [0032](0032-cross-node-invocation-and-what-the-hop-costs.md) | Cross-node invocation works, and the hop is nearly free (~4%) | accepted |
| [0033](0033-two-orgs-under-load.md) | Two organisations under load: what the platform costs and whether it holds | accepted |
| [0034](0034-two-machines-one-fleet.md) | Two machines, one fleet: placement does not map tenants to computers | accepted |
| [0035](0035-losing-a-machine.md) | Losing a machine, measured through the failure | accepted |
| [0036](0036-open-loop-stress-and-a-correction.md) | Open-loop stress from a third machine, and a correction to 0033/0034 | accepted |
| [0037](0037-what-a-cold-start-costs.md) | What a cold start costs, and why scale-to-zero is affordable | accepted |
| [0038](0038-autoscaling-on-observed-concurrency.md) | Autoscaling on observed concurrency (min/max/target) | accepted |
| [0039](0039-comp-versus-wasmcloud.md) | comp vs wasmCloud 2.x, same component, both machines | accepted |
| [0040](0040-compiled-artifacts-are-cached.md) | Compiled artifacts are cached (81x faster starts) | accepted |
| [0041](0041-the-ingress-sheds-load.md) | The ingress sheds load instead of queueing without bound | accepted |
| [0042](0042-scale-to-zero-and-back.md) | Scale to zero, and back — a request activates a parked app | accepted |
| [0043](0043-placement-weighs-capacity.md) | Placement weighs capacity, not just instance count | accepted |
| [0044](0044-subjects-carry-a-version.md) | Subjects carry a version | accepted |
| [0045](0045-shedding-feeds-autoscaling.md) | Shedding feeds autoscaling — a refused request is unmet demand | accepted |
| [0046](0046-what-the-signal-cannot-say.md) | What the signal cannot say — wedged vs saturated, at-ceiling, and absent vs idle | accepted |
| [0047](0047-config-is-declared-and-checked.md) | Config is declared by the uploader and checked at save | accepted |
| [0048](0048-does-this-plug-fit.md) | Does this plug fit? — the real subtype check, and typed request bodies | accepted |
| [0049](0049-the-org-can-see-it.md) | The org can see it — ADR-0007's middle row, and a market endpoint | accepted |
| [0050](0050-secrets-by-reference.md) | Secrets, by reference — stored, validated, not yet readable at runtime | accepted |
| [0051](0051-the-secret-reader.md) | The secret reader — a key, a handle, and one explicit reveal | accepted |
| [0052](0052-one-copy-per-digest.md) | One copy of the machine code per digest | accepted |
| [0053](0053-the-matrix.md) | The matrix, and the number it corrected | accepted |
| [0054](0054-pooling-on-and-the-leak-that-was-not.md) | Pooling is on by default, and the leak was not a leak | accepted; closes what [0053](0053-the-matrix.md) left open |
| [0055](0055-how-the-control-loop-scales.md) | How the control loop scales, and the tenancy bug that found | accepted |
| [0056](0056-a-converged-app-keeps-its-placement.md) | A converged app keeps its placement | accepted; extends [0055](0055-how-the-control-loop-scales.md) |
| [0057](0057-the-latency-column-was-arithmetic.md) | The latency column was arithmetic, and the rps column was NATS | accepted; **corrects the latency figures in earlier runs** |
| [0058](0058-snapshots-compress-and-parses-are-reused.md) | Snapshots compress, parses are reused, and the watch was not built | accepted |
| [0059](0059-the-read-mirror-lost.md) | The read mirror lost, 2.3× | **rejected** — the code is not in the tree |
| [0060](0060-the-ingress-forgot-what-it-was-told.md) | The ingress forgot what it had just been told | accepted |
| [0061](0061-the-secret-reader-was-never-linked.md) | The secret reader was never linked | accepted; **corrects [0051](0051-the-secret-reader.md)**, which said built and was half-built |
| [0062](0062-what-a-real-application-asks-the-store-for.md) | What a real application asks the store for: 264 reads per write | accepted as a measurement; **shows [0059](0059-the-read-mirror-lost.md)'s rejection was workload-specific** |
| [0063](0063-a-ttl-is-cheaper-than-coherence.md) | A TTL is cheaper than coherence — durable reads at in-memory speed | accepted, built, **off by default**; answers [0059](0059-the-read-mirror-lost.md) |
| [0064](0064-the-cross-node-cost-of-the-read-cache.md) | The cross-node cost of the read cache, measured | accepted as a measurement; **discharges the gap [0063](0063-a-ttl-is-cheaper-than-coherence.md) named** |
| [0065](0065-the-cache-defeats-the-revision-guard.md) | The cache defeats the revision guard — a measured lost update | accepted as a finding; **why [0063](0063-a-ttl-is-cheaper-than-coherence.md) stays off by default** |
| [0066](0066-the-guard-moves-into-the-store.md) | The guard moves into the store — `comp:store/cas` | accepted, and built; **fixes [0065](0065-the-cache-defeats-the-revision-guard.md)** |
| [0067](0067-one-copy-is-not-a-backup.md) | One copy is not a backup — replication and a restore that works | accepted, and built; first measurement of surviving the loss of the STORE |
| [0068](0068-the-index-was-the-lossy-part.md) | The index was the lossy part — a guarded id list, `repair`, and a corrupted `list-keys` | accepted, and built |
| [0069](0069-what-wasmcloud-does-with-keys.md) | What wasmCloud does with keys: nothing — and what was worth taking | accepted; confirms [0068](0068-the-index-was-the-lossy-part.md), adopts their CAS backoff |
| [0070](0070-a-rate-limit-is-not-a-record.md) | A rate limit is not a record — 85 store operations per request, then 2 | accepted, and built |
| [0071](0071-a-captured-fetch-is-spent.md) | A captured fetch is spent — replay protection, and `repair` finished | accepted, and built |
| [0072](0072-one-loop-at-a-time.md) | One loop at a time — leader election, and why not sharding | accepted, and built |
| [0073](0073-public-costs-a-signature.md) | Public costs a signature — ADR-0007 rule 3, eleven ADRs later | accepted, and built |
| [0074](0074-the-split-graph-still-works.md) | The split graph still works — restoring the test [0032](0032-cross-node-invocation-and-what-the-hop-costs.md) lost | accepted, as a test |
| [0075](0075-silence-is-not-health.md) | Silence is not health — drift on the read path, and a `verify` that only reports | accepted, and built |
| [0076](0076-revocation-without-versions.md) | Revocation without versions — and why per-version keys stay unbuilt | accepted, and built |
| [0077](0077-asking-the-same-question-twenty-times.md) | Asking the same question twenty times — the `feed` N+1, removed | accepted, and built |
| [0078](0078-an-environment-is-a-derived-app.md) | An environment is a derived app — parallel exploration, and why not a host per branch | accepted; desired-state half built |
| [0079](0079-a-component-forks-its-own-app.md) | A component forks its own app — the instance token as identity | accepted; platform half built |
| [0080](0080-the-graph-remembers.md) | The graph remembers — a knowledge graph over SurrealDB, as a component | accepted; proven against a live database |
| [0081](0081-fitness-fuel-and-what-the-swarm-knows.md) | Fitness, fuel, and what the swarm knows — judging a branch, sharing knowledge, and stopping | **proposed**; §2 (knowledge) built as ADR-0084, the rest unbuilt |
| [0082](0082-a-project-owns-a-repo-and-a-queue.md) | A project owns a repo and a queue — one repo, a dead-letter queue, and a human starts every goal | accepted; the queue is built, the runner is not |
| [0083](0083-an-environment-needs-a-door.md) | An environment needs a door — the ingress [0078](0078-an-environment-is-a-derived-app.md) said an environment would not need | accepted; amends [0078](0078-an-environment-is-a-derived-app.md) |
| [0084](0084-two-retrievers-and-an-optimistic-database.md) | Two retrievers and an optimistic database — `knowledge:memory`, KNN in SurrealDB, `+=` over read-modify-write | accepted; 9 scenarios + a 5-component composed e2e pass; the goal runner skips work already done, retrieval is not wired yet |
| [0086](0086-parts-negotiate-a-contract.md) | Parts negotiate a contract | **built**, demonstrated end to end, and reachable from a goal spec |
| [0087](0087-a-composition-is-derived-not-written.md) | A composition is derived, not written | **built**, and used by every clinic gate — `just plug <name>` |
| [0088](0088-what-a-gate-says-is-what-the-next-attempt-reads.md) | What a gate says is what the next attempt reads | accepted as a rule, with two mechanical guards behind it |
| [0089](0089-capability-accumulation.md) | Capability accumulation | **proposed**, about half built — reuse enforced; discovery, promotion and duplicate detection missing |
| [0091](0091-one-store-one-schema.md) | One store, one schema | **accepted**, slice one built and live-verified; supersedes [0085](0085-structure-flows-down-lessons-flow-up.md), reverses [0090](0090-a-lesson-is-about-a-capability-not-a-sentence.md) |
| [0092](0092-a-run-leaves-a-trace.md) | A run leaves a trace | **accepted**, and built — the event vocabulary [0091](0091-one-store-one-schema.md) deferred |
| [0093](0093-a-lesson-is-reached-through-its-interface.md) | A lesson is reached through its interface | **accepted**, and built — the `lesson -about-> interface` edge [0091](0091-one-store-one-schema.md) drafted |
| [0094](0094-a-capability-describes-itself-in-a-callers-words.md) | A capability describes itself in a caller's words | **accepted**, and built — the discovery third of [0089](0089-capability-accumulation.md) |
| [0095](0095-what-is-allowed-to-be-native.md) | What is allowed to be native | **accepted** — names the rule the tree already followed, and the five things that qualified |
| [0096](0096-a-pull-contract-needs-a-relay.md) | A pull contract needs a relay, and the relay is native | **accepted**, and built — `comp-relay`, the sixth native thing |
| [0097](0097-a-derived-file-is-a-liability.md) | A derived file is a liability, and a name is not a fact | **accepted**, and done — `catalog.json` deleted, `reusable_as_is` read off the exports |

## History: superseded, and kept

ADR-0001's rule is *superseded rather than edited*, so nothing here is deleted and
nothing above is rewritten to look wiser than it was. These are how the platform
got to the shape it has, and each names what replaced it. Read them when
you want to know why something is the way it is; read the table above when you want
to know what is true.

| # | decision | replaced by |
|---|---|---|
| [0002](0002-tenant-is-a-namespace.md) | A tenant is a Kubernetes namespace | superseded by [0021](0021-there-is-no-kubernetes.md) |
| [0003](0003-control-plane-is-wasm-plus-applier.md) | The control plane is a wasm app plus a small native applier | applier half superseded by [0022](0022-desired-state-is-a-manifest.md); the split itself stands |
| [0004](0004-reconcile-by-server-side-apply-on-save.md) | Reconcile by server-side apply on save | superseded by [0022](0022-desired-state-is-a-manifest.md) |
| [0008](0008-isolation-is-stamped-never-authored.md) | Isolation is stamped by the platform, never authored by tenants | superseded by [0023](0023-isolation-is-a-linker-boundary.md); its release gate re-met in [0026](0026-the-adversarial-run.md) |
| [0011](0011-slice-one-scope.md) | Slice 1 is single-tenant, both strategies, one cluster | superseded by [0025](0025-slice-one-on-the-lattice.md) |
| [0013](0013-unenforceable-capabilities-are-denied-by-omission.md) | A capability the host cannot partition is denied by omission | superseded by [0023](0023-isolation-is-a-linker-boundary.md) |
| [0014](0014-an-application-owns-a-host.md) | An application owns a host | superseded by [0023](0023-isolation-is-a-linker-boundary.md) |
| [0016](0016-deleting-an-app-is-reconciled-not-remembered.md) | Deleting an app is reconciled, not remembered | reaping half superseded by [0021](0021-there-is-no-kubernetes.md) |
| [0017](0017-the-applier-pushes-and-the-registry-is-a-cache.md) | The applier pushes, and the registry is a cache | superseded by [0022](0022-desired-state-is-a-manifest.md) — the applier is gone from the tree; the registry-is-a-cache rule survives as [0024](0024-artifacts-are-content-addressed.md) |
| [0018](0018-the-platform-deploys-a-running-app.md) | The platform deploys a running app, and what that took | **historical**: the first end-to-end deploy, on machinery [0021](0021-there-is-no-kubernetes.md) removed; the claim re-met on the lattice by [0025](0025-slice-one-on-the-lattice.md) |
| [0085](0085-structure-flows-down-lessons-flow-up.md) | Structure flows down, lessons flow up | superseded by [0091](0091-one-store-one-schema.md) — one store, not two pools |
| [0090](0090-a-lesson-is-about-a-capability-not-a-sentence.md) | A lesson is about a capability, not a sentence | title stands, central claim reversed by [0091](0091-one-store-one-schema.md) — two truth models needed two lifecycles, not two stores |

## The shape these add up to

For the shape that runs **now** — nodes, a reconciler, an ingress, and where a
component's isolation boundary is — read [`../CURRENT.md#shape`](../CURRENT.md#shape).
The Kubernetes-era diagram that used to be here is archived with the rest of that
world in [`../archive/slice-one-kubernetes-status.md`](../archive/slice-one-kubernetes-status.md).

## Where the rest went

- **What is true now**, rather than how it was decided: [`../CURRENT.md`](../CURRENT.md).
- **The slice-one status and open-risk tables** that used to end this file are
  archived at [`../archive/slice-one-kubernetes-status.md`](../archive/slice-one-kubernetes-status.md).
  Every row was about Kubernetes, the applier, or `NetworkPolicy` — the world
  [0021](0021-there-is-no-kubernetes.md) ended.
- **Everything else in `docs/`**, and which files are generated: [`../README.md`](../README.md).
