# ADR-0084 — two retrievers, and an optimistic database

*Knowledge between apps and hosts: `knowledge:memory`, where the vector search
happens, how duplicated work is skipped, and what 20-way concurrency on one hot
key actually does.*

**Status:** accepted, and built — slice one of three. Nine scenarios and a five-component composed e2e pass; retrieval into a branch's prompt (slices two and three) is not wired.

## The gap this closes

ADR-0080 gave the loop a place to remember. ADR-0081 designed what should be
remembered and by whom, and recorded its own blocking dependency:

> `llm:inference/embed` exists and, like everything else in that stack, has never
> been called. **That is the dependency to close first** — without retrieval, a
> knowledge store is a write-only log.

Three stores existed and none of them was knowledge management: `knowledge:graph`
remembers and traverses, `search:index` ranks lexically, `llm:inference/embed`
turns text into a vector. What was missing was not storage. It was **policy** —
who may write what the swarm believes, how a lesson earns its place in a prompt,
and how much of it any one branch may read.

`components/knowledge-memory` is that policy and nothing else: three contracts in,
two out, no host capability, no config, no secret.

## The write rule is a linker boundary, not a check

ADR-0081's rule — *an agent may record what it observed; only a passing gate may
promote something to what the swarm believes* — is enforced by the shape of the
world rather than by a flag:

    interface memory     { observe, recall, attribute }    ← the agent's world
    interface promotion  { promote }                        ← the post-gate hook's

An agent linked to `memory` has no verb that reaches `patterns`. This is the same
move as the rest of the repo (a branch's store is named after its derived app, so
isolation is a linker boundary — ADR-0078), and it costs one extra interface.
`observe` refuses `patterns` too, so a mis-linked world fails loudly rather than
quietly writing the trusted pool.

Two smaller applications of the same instinct, both found while writing it:

- **`promote` forces the namespace.** Letting a caller name it would give the
  post-gate hook the ability to write a lesson nobody could tell from an
  observation.
- **A row whose `ns` property cannot be decoded reads as `solutions`, never
  `patterns`.** A decoding gap must not promote anything.

## Where the vector search happens: in the database

The first draft of this ADR kept dense retrieval out of the database — cosine in
the component, over candidates the lexical index had already found — on the
grounds that an ANN index was the expensive half to build. Running the statements
against a pinned SurrealDB v3.1.3 killed that argument: **the ANN index is one
idempotent DDL statement, and nearest-neighbour search works without it at all.**

    goal ─┬─▶ search:index/query per pool ──▶ sparse candidates (k×4) ─┐
          │                                                            ├─▶ RRF fusion
          └─▶ SELECT *, vector::distance::knn() AS dist                │   × outcome weight
              FROM memory WHERE vec <|k×4,COSINE|> [q] AND ns IN [...] ┘

So retrieval is **two retrievers fused**, not one retriever reranked:

- **dense** — one statement. SurrealDB does the search and returns whole rows, so
  those candidates arrive hydrated; the pool is never dragged through this
  component to be compared here. Measured: `<|k,COSINE|>` works on an unindexed
  table (brute force), and `<|k,ef|>` uses the HNSW index when one exists.
- **sparse** — `search:index`, TF-IDF over the KV store, no network. The half that
  keeps working when the embedding provider is down or has moved underneath the
  index.

Fusing rather than layering matters because the two fail differently: dense finds
a lesson that shares no words with the goal (which lexical-first recall could never
surface), and sparse finds one whose vector came from a model that has since
changed. Reciprocal rank fusion needs no calibration between the two scales, which
is why it is the fusion and not a weighted sum.

Only the lexical candidates the dense pass did not already return are hydrated,
and in **one** statement — one read per candidate is the N+1 of ADR-0077, and a
retrieval path would do it k×4 times per branch.

### The model-drift guard is now a database constraint

The original worry stands and is worth restating:

> A hosted embedding model changes under you and **nothing errors**. Vectors
> stored last month come from a different space than vectors computed today, and
> the cosine between them is not an error — it is a number, and it is meaningless.

The HNSW index carries its `DIMENSION`, and SurrealDB refuses anything else —
measured: *"Incorrect vector dimension (2). Expected a vector of 4 dimension."*
So `DEFINE INDEX IF NOT EXISTS … DIMENSION <d>` travels with the first write, and
a model change of a different width becomes a **loud write-time rejection** rather
than an index quietly holding two incompatible spaces. That is strictly better than
the app-side dimension check the first draft settled for.

The rejection is then handled rather than reported: the write is resent **without
its vector** and flagged `dim_conflict`. Losing a lesson because an embedding model
moved is worse than losing dense retrieval of it, and a flag is queryable where a
dropped write is not.

Also measured, and the reason the read path needs no guard of its own: brute-force
KNN **silently skips** vectors of the wrong width rather than erroring. Silent is
acceptable on a read whose write path is loud.

Two structural choices survive unchanged from the first draft, and neither is a
model choice:

1. **The embedded text carries a context header** — the goal, then the lesson. A
   lesson embedded alone is unfindable; `slugify a string — prefer char_indices`
   is findable both ways. Worth more than the choice of embedding model.
2. **The reported `similarity` is the cosine, not the fused score**, so
   `min-similarity` means the same thing every time it is read. It is a caller's
   knob because it is per-model by nature: one tuned on one model is a
   superstition on another.

`vector::distance::knn()` answers a **distance** (0.0 is an exact match) and every
threshold here is a **similarity**. The conversion happens in one named function,
because getting it backwards gives you a `min-similarity` that filters nothing.

An embedding provider remains an **optional capability behind a required import**.
`describe()` is asked before every embed; `anthropic-provider` refuses `embed`
rather than faking one, and a deployment linking it gets sparse-only retrieval
instead of an error. `dense: false` on every hit says so.

## Duplicated work: every evaluation is recorded, passing or not

`evaluated(goal, run, score, passed, artifact)` is called on every gate verdict —
**once per branch**, because the count of failed attempts on a goal is what says
whether another generation is worth buying, and a generation-level record cannot
say it. It upserts a `task` node keyed by the hash of the normalised goal and
draws an `evaluated_by` edge to the run carrying that verdict's score. Only a
**passing** verdict overwrites the winner fields — score, run, artifact — so
`already-done` can never hand back work that merely got attempted.

The edge id is deterministic, `<task>|<run>`, and `RELATE` with an explicit id is
an upsert rather than a duplicate-key error (measured). So the verb is idempotent
per `(goal, run)`: the landing path re-reports the winning run with the pull
request the forge opened, and no fifth evaluation appears. Both counts are then
**derived from the edges** rather than stored, which is the only way they can be
right across a fan-out of branches that each report and may each retry.

`already-done(goal, min-similarity)` is then one KNN statement over the goals that
have passed, with a floor of 0.9 by default. Two things about it are worth pinning:

- **The KNN always returns a row** when the table is non-empty — asking about an
  unrelated goal came back with `dist: 1.0`, an orthogonal vector, rather than with
  nothing. The floor is what makes the answer correct, not the query, and 0.9 is
  high on purpose: skipping work that was not actually done is a silent wrong
  answer, where redoing work is only money.
- **Without an embedding provider it degrades to an exact match** on the normalised
  goal. That still catches the duplicated work that dominates in practice — a
  retried generation asking for the same thing again — and it needs no vectors at
  all.

The failure counts are the other half of the value: a goal five runs have failed is
knowledge too, and it is `evaluations` that says whether a sixth attempt is worth
buying. The per-verdict trail reads off the edges (`SELECT ->evaluated_by.* FROM
task:⟨…⟩`) with no run node needed — measured: `RELATE` against a missing record
creates the edge only, and does **not** materialise the end.

## Contention: what 20-way concurrency on one hot key actually does

The question this design has to answer is "how do twenty branches write one pool
without a lock and without wedging". Measured against SurrealDB v3.1.3, 60
concurrent writers, 20 in flight, one record:

| strategy | of 60 increments, how many land |
|---|---|
| read-modify-write from the component (`SELECT`, add one, `UPDATE`) | **7** |
| `UPSERT … SET uses += 1`, no retry | 53 — the other 7 rejected as conflicts |
| `UPSERT … SET uses += 1` + resend on conflict | **60, exactly** (53 first-try, 7 second-try, none needed a third) |

That is the whole strategy, and it is three claims:

1. **The comparison happens where the data is.** Read-modify-write across the
   component boundary lost 88% of its writes — the same failure ADR-0065 measured
   for `record-store`'s revision guard, arrived at from the other direction. So
   `attribute` never reads a counter, and neither does `observe`: `SET uses += 0`
   creates the field when it is absent and preserves it when it is not, which is
   how re-observing a lesson keeps the standing it earned **without a read at
   all**.
2. **SurrealDB does not deadlock; it aborts the loser.** Transactions are
   optimistic. The rejection says so in words — *"Transaction conflict: Write
   conflict, retry the transaction. This transaction can be retried"* — and a
   long-running transaction is the one that loses: a `BEGIN; UPDATE; SLEEP 300ms;
   COMMIT;` was aborted wholesale by a short transaction that started later. There
   is no lock to hold, so there is no lock ordering to get wrong, and no advisory
   lease (`lock:mutex`) is needed for this.
3. **A resend is safe for a non-idempotent statement**, because a conflicted
   transaction did not commit. This is why the retry can sit in the shared path
   and cover `SET uses += 1`. The evidence is the final counter reading exactly 60
   rather than more.

The retry therefore lives in **`knowledge:graph`**, not here — `send()` in front of
every statement, four attempts, **no backoff**. Nothing to back off from: the winner
committed before the loser heard about it, so the contended record is already free.
Fixing it in the shared path also closed a real gap: `query` — the escape hatch —
previously bypassed the namespace-bootstrap retry as well, so a component doing its
writes through raw SurrealQL (this one does, for `+=` and KNN) got neither
behaviour. Both now sit on the one path all four typed verbs and the escape hatch
share.

Three more contention notes, from the same measurements:

- **Spread keys never conflict.** 60 concurrent writes to 60 different records:
  60/60, zero retries. Contention is a hot-key property, and the hot keys in this
  design are the shared entries every branch reads — which is exactly why their
  counters move by `+=`.
- **Edge fan-in does not conflict.** 60 branches drawing `used_in` edges to one run
  node: 60/60. `RELATE` writes a new edge record each time rather than mutating
  either end.
- **A whole verdict is one transaction.** `attribute` batches every key into one
  `BEGIN … COMMIT`, so a run's outcome lands atomically or not at all — and a
  failure inside a transaction takes the whole body down, which is why the response
  reader here treats an error in *any* statement as an error rather than reading
  the last result and shrugging.

## Standing comes from outcomes, and only from outcomes

There is no confidence field, because a self-reported confidence is a number an
agent optimises against. `attribute(keys, run, succeeded)` is the only thing that
moves an entry, and it moves it through a multiplier:

    fused_score *= 0.5 + 0.5 * (wins / uses)      // neutral 0.75, floor 0.5

An entry that keeps being present when runs fail sinks, and nothing had to decide
how confident it was. The counters live on the node and the `used_in` edges are
the record they are a denormalised read of — one `relate` call writes both, since
`relate` upserts its ends anyway.

`recall-opts` is the **diversity budget** rather than a cost knob: `k`, a character
budget, which pools, and the threshold. A generation whose branches all read the
same top-k is an expensive way to run one branch (ADR-0081's herding), so the
driver varies these across siblings — and `k = 0` returns nothing, spelled
explicitly, so a cold control arm is free.

## Nine scenarios, and what each one is worth

`components/knowledge-memory/src/scenarios.rs` starts the pinned container and runs
**the statements `surql.rs` builds** — the same strings the component sends, not
re-spelled ones, which is why the builders were moved into a module whose signatures
carry no generated bindings. Nine graph shapes, each with an expected finding:

| # | the graph, and the outcomes in it | expected finding | why it is in the suite |
|---|---|---|---|
| 1 | nothing written at all | `already-done` → none; `recall` → empty, no error | the first read of a project always precedes its first write |
| 2 | one goal, one passing run | similarity ~1.0 → skip, `evaluations = 1` | the duplicate that actually dominates: a retried generation |
| 3 | that pool, asked with a paraphrase and with a stranger | paraphrase reuses the work; stranger does **not** | the KNN returns its nearest row regardless — the floor is what makes the answer correct |
| 4 | one goal, three runs, all failed | not skipped, `evaluations = 3`, `passes = 0`, three verdicts on the edges | a goal that keeps failing is knowledge; it is not finished work |
| 5 | two lessons, opposite outcome histories | the lesson present when runs passed outranks the one present when they failed — and the loser sinks rather than disappears | ordering moves only because `attribute` recorded outcomes; nothing asserts a confidence |
| 6 | one pool, four branches | three branches with identical options read an **identical** prompt; a different pool mix reads a different one; `k = 0` reads nothing | herding, made visible — ADR-0081's failure mode that does not announce itself |
| 7 | 20 branches × 3 attributions on one entry | `+=` with resend: 60/60 and 60 edges, no duplicates. Read-modify-write: **7/60** | the contention table, as an assertion rather than a claim |
| 8 | an entry written at width 16, then at width 32 | the wider vector is refused, recognisably; the lesson lands with `dim_conflict = true` | drift is loud, and a lesson is never lost to it |
| 9 | 20 candidates to hydrate | one statement returns all 20, and is not slower than 20 | the ADR-0077 N+1, which a retrieval path would run k×4 times per branch |

Run it with `cargo test -p knowledge-memory -- --nocapture scenarios`. It prints:

```
=== knowledge:memory — what nine scenarios saved ==========================
duplicated work     2/5 goals answered from a past passing run, 0 false skips
                    → 8 branches never spawned  ≈ $0.16 at $0.02/branch (ASSUMED)
retrieval reads     20 round trips → 1 for the same candidates (20x fewer)
hot-key writes      read-modify-write kept 7/60; `+=` with retry kept 60/60 (1 resend)
model drift         1 lesson kept when the embedding width changed (0 = data loss)
==========================================================================
```

**What is measured and what is assumed.** Every count is asserted. The money is
not asserted by anything: it is `goals_skipped × 4 branches × $0.02`, and the two
multipliers are a stated assumption — a generation is four branches (ADR-0078) and
the README's end-to-end goal cost "a few cents". The honest form of the saving is
therefore **the count**: two of five goals needed no generation at all, and 8
branches were never spawned. What a branch costs is the deployment's number, and
`fuel` (ADR-0081) is where it will come from.

The 7/60 is worth dwelling on: it reproduces, from a different harness, exactly the
number the by-hand measurement produced. Read-modify-write from outside the database
loses 88% of its writes under 20-way contention, and the scenario asserts that it
*does* lose them — a run where the naive arm scored 60/60 would mean the test had
stopped proving anything.

**The fixture caught its own version of this bug.** On the first run, the cold-pool
scenario passed while the suite was talking to a database with no namespace defined:
every statement answered "The namespace 'comp' does not exist", which the response
reader maps to *empty* on purpose. So the fixture now mirrors `send()`'s bootstrap
and asserts, before any scenario runs, that a write it makes reads back. A fixture
that cannot tell "no rows" from "no database" makes half a suite pass for free.

## Composed through a host: five components, four links

`reconciler/tests/memory.rs` deploys the whole thing on a real fleet —
`memory-probe` → `knowledge-memory` → {`knowledge-graph` → a real SurrealDB,
`search-index` over the host's key-value store, `mock-provider`} — and drives it
over the ingress. 8.7s, skipping loudly without Docker.

It asserts only what the layers below cannot:

- **The host links three non-`wasi` component interfaces into one caller.** This is
  the fixture that would catch ADR-0079's `HOST_NAMESPACES` bug shape ("wasi:*
  links, a component interface silently does not"), and one POST proves all four
  links at once: a missing one is a trap, not a wrong answer.
- **`llm:inference/embed` is reachable, and was called.** `dense: true` on a hit can
  only be true if the provider embedded, the vector reached SurrealDB, and the KNN
  found the row by it. **Nothing in this repo had ever called that function** — it
  was the dependency ADR-0081 said to close first, and it is now closed through a
  host rather than in a unit test.
- **`search:index` answers over the host's store**, so a query whose only overlap
  with a lesson is lexical still finds it. The sparse half of retrieval is no
  longer a rank list handed in by a test.
- **The two exported interfaces are separately linkable.** `observe` into
  `patterns` is refused and `promote` at score 1000 lands, from one caller, over
  HTTP. The anti-poisoning argument was a linker claim; now it is a linked
  deployment.
- **A second app with `mock-embeddings=false`** proves the degraded path: writes
  still land, recall still retrieves, every hit says `dense: false`, and
  `already-done` falls back to the exact-goal match.

`memory` declares nothing in the manifest — no config, no secret, no host
capability. It is policy over three contracts, so if the host hands it no links it
cannot start at all, which is a property worth having in a fixture.

## What worked

- **Running every statement against the pinned database before writing the Rust.**
  ADR-0080's lesson, applied deliberately this time, and it changed three
  decisions: the ANN index went from "too expensive to build" to one DDL line, the
  dimension guard moved from app code into a database constraint, and the counter
  strategy changed outright. Every shape below is now pinned by a unit test
  carrying the captured JSON.
- **Reusing the stores unchanged.** No new host capability, no host rebuild, no new
  database, no new dependency. `knowledge:graph` still owns the connection, the
  credentials, the namespace bootstrap, the egress boundary and now the retry.
- **Optional dense retrieval.** Because sparse is a full retriever rather than a
  fallback, the write path and the read path could both be finished and tested
  before anyone decided which embedding provider to pay for.
- **The policy as pure predicates.** `agent_may_write`, `promotion_allowed`,
  `weight`, `rrf`, `trim`, `rows_of`, `similarity_of`, `dimension_conflict` are
  free functions over plain data, so the rules that matter are covered by 17 native
  tests with no runtime, no database and no provider — and `knowledge:graph`'s
  conflict handling by 10.
- **One shared statement path.** Putting the retry in `send()` rather than in this
  component means the four typed verbs, the escape hatch and every future caller
  get it, which is also how the escape hatch stopped being the one path with no
  namespace bootstrap.
- **The composition needed no host change.** Five components, four links, two
  exported interfaces of one component linked separately, three non-`wasi` imports
  in one caller — and the composer did it with nothing rebuilt and no flag added.
  Worth recording because the last two ADRs about linking (0079, 0080) each ended in
  a host fix, and this one did not.
- **Two apps in one fleet, sharing artifacts.** The dense and sparse fixtures place
  the same five wasm files and differ only in a manifest, which is what made the
  degraded path cheap enough to test at all. Store isolation held: the sparse app
  cannot read the pattern the dense app promoted, asserted rather than assumed
  (ADR-0023).

## What did not work

- **Read-modify-write, which is what the first implementation did.** `observe` read
  an entry to preserve its counters and `attribute` read one to increment it. Under
  the measurement that is a 7-in-60 write path, and it was written *knowing*
  ADR-0065 had already found the same bug in `record-store`. Both reads are now
  gone; nothing in this component holds a counter in a variable.
- **`include` is a WIT keyword.** `recall-opts.include` would not parse; the field
  is `pools`. Trivial, and the kind of thing only a build finds.
- **`embed` cannot say which model answered.** `completion` carries `model`;
  `embed` returns a bare `list<f32>`. So the model identity ADR-0081 wanted stored
  beside every vector **cannot be obtained from the contract**. The database's
  `DIMENSION` constraint covers a width change, which is the common case, but a
  same-width model swap still slips through. Changing the shared interface would
  break every provider, so the gap is recorded and a canary set is marked
  `ponytail:` in the source rather than built on speculation.
- **`UPDATE` is not `UPSERT`, and the difference is load-bearing.** `UPDATE` on a
  record that does not exist is a no-op; `UPSERT` creates it. `attribute` uses
  `UPDATE` on purpose so a handle a human has deleted stays deleted — and a first
  draft using `UPSERT` would have resurrected it as a node with counters and no
  lesson.
- **A KNN query with a floor is not a KNN query.** `<|1,COSINE|>` returns the
  nearest row even when nothing is near — `dist: 1.0` for an orthogonal query — so
  a `already-done` that trusted the query to return nothing would skip unrelated
  work. The floor does the work.
- **The graph's `query` escape hatch inherited none of the lessons the typed verbs
  learned** (ADR-0080): no namespace bootstrap, no conflict retry, and a missing
  table surfacing as a failure. The first two are fixed at the source in
  `knowledge:graph`; the third is re-derived here, because the first `recall` of a
  project always precedes the first `observe` and an empty pool reading as a broken
  one is the first thing anyone would hit.
- **`entry.key` is a trust boundary, and the graph's quoting is not exported**, so
  the angle-bracket quoting is duplicated here — five lines, and a test that a key
  carrying `⟩; DELETE memory` cannot end the quoting early. Text values go through
  JSON re-serialisation for the same reason.
- **The test double could only say yes.** `mock-provider`'s `describe()` returned
  `(model, true)` unconditionally, so the sparse-only path — the one
  `anthropic-provider` really takes, having no embeddings endpoint — could not be
  exercised without an API key and a live vendor. It now reads a
  `mock-embeddings` config knob and refuses `embed` when it says no. A mock that
  cannot fail the way the real thing fails is a mock that certifies the happy path.
- **Without a provider, a paraphrase is not recognised.** Asserted in the sparse
  app: the exact-goal match still skips repeated work, but "make a slug from **the**
  title string" reads as new work. That is the measured cost of deploying with no
  embedding model, and it belongs in the deployment decision rather than in a
  footnote.
- **A pinned image had started to fork.** `graph.rs` owned the SurrealDB container
  fixture, and the second suite would have copied it — two places to forget to pin a
  version, which is the whole reason it is pinned. It moved to
  `reconciler/tests/harness/`, where the control plane fixture already lives for the
  same reason (ADR-0073).
- **Counters could not survive a fan-out.** The first cut bumped
  `evaluations += 1` per verdict, which meant the landing path could not re-report
  the winner with its pull request without inventing a fifth evaluation. Fixed by
  making the verdict an EDGE with a deterministic `<task>|<run>` id — measured:
  `RELATE` with an explicit id is an upsert, not a duplicate-key error — and
  deriving both counts from the edges. `evaluated` is now idempotent per
  `(goal, run)`, which is what a fan-out of branches that each report, and may
  retry, actually needs.
- **A fast harness disagreeing with a slow one meant they were not running the same
  code.** The composed e2e counted five verdicts where four were expected; the
  scenario reproduction passed in 1.3s. The e2e was driving a wasm artifact built
  before the change. A component edit needs a rebuild before any fleet suite means
  anything, and the 1.3s test that localised it is now a permanent one.
- **One table, not one per namespace.** The first draft used a table per pool,
  which meant a handle did not identify an entry and `attribute` needed a second
  argument saying where to look. The namespace is a property; the handle is
  `<ns>:<key>` and round-trips from `recall` to `attribute` untouched.

## What this does not do yet

- **Nothing calls it.** The driver does not `recall` before generating, the
  post-gate hook does not `promote`, and the selector does not `attribute` on the
  verdict. Until those three edges exist this is a capability, not a behaviour —
  and this repo has shipped an unlinked component before (ADR-0061), so it is said
  plainly rather than implied.
- **No decay.** ADR-0081 already caught alpha-swarm2 exposing `decay` and never
  scheduling it. Nothing here schedules one either, which means the honest state
  is: no retention, same as ADR-0080.
- **No export to the repo.** `.swarm/memory/**` as reviewable markdown — so a
  human can delete a pattern they disagree with in a pull request — is a good idea
  that is not built.
- **Herding is mitigable but not measured.** The knobs to vary retrieval across
  siblings exist; nothing yet reports that a generation converged, which ADR-0081
  and `.comp/goals/03` both ask to be as loud as a failure.
- **No canary, and no re-embedding path.** A width change is now a loud rejection
  and a `dim_conflict` flag, but nothing re-embeds the corpus afterwards, and a
  same-width model change is still invisible.
- **Only the first slice is wired.** `holon goal run --surreal-url …` now asks
  `already-done` before spawning anything and records every branch's verdict on the
  way out, re-reporting the winner with the pull request once the forge opens one.
  What is still unwired is retrieval: no branch reads a lesson into its prompt, no
  gate hook promotes, no selector attributes on the verdict. That is slices 2 and 3,
  and slice 2 touches the spawn path (`.comp/goals/03` thread 3).
- **Opt-in, and that is a decision.** No `--surreal-url`, no memory app, no calls,
  and the loop is byte-for-byte what it was. Requiring a database for every real run
  would trade a loop that works for a loop that needs a database to be up, and
  ADR-0080 already recorded that nothing here deploys, backs up or watches one.
- **`already-done` is not called on the spawn path**, so nothing yet skips anything.
  The verb exists and answers correctly; the saving is not banked until the driver
  asks it before spending a generation.
- **Retention is still nothing.** No decay, and now two tables growing instead of
  one.
