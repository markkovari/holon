# ADR-0092 — a run leaves a trace

*`comp-goalrun` prints to stdout. When the terminal closes, the answer to "why
did branch 3 beat branch 7, and what did either of them read" is gone. The
lessons survive; the run that produced them does not.*

**Status: accepted.** This is the events ADR [ADR-0091](0091-one-store-one-schema.md)
deferred — *"the event vocabulary and outcome counting are append-only and cheap
to change; writing them down now would record a guess as a decision."* Slice one
has run, so it is no longer a guess.

## The gap

ADR-0091 named `run`, `attempt` and `event` as accumulated nodes and settled that
a run's subgraph is a **query over its events**, not stored edges. Nothing writes
any of them. What exists today is:

* **lessons** — what a run concluded, in `knowledge-memory`, gate-blessed and
  retrievable (ADR-0084).
* **evaluated goals** — that a goal was attempted and what score it reached
  (`already-done`).

Both are *distillate*. Neither answers "what happened". A branch that failed
teaches nothing retrievable and leaves no record it existed, so the questions a
person actually asks after a run — which branch won, how many attempts each took,
what the gate said, which lessons were read — have no data behind them.

## Not `observe`, and not a lesson

The obvious shortcut is `knowledge:memory/observe`: it already appends, already
reaches the store, already has a transport in `reconciler/src/memory.rs`.

It is the wrong door. ADR-0084's whole design is that `observe` writes what a
branch *believes*, and `promote` — reachable only downstream of a passing gate —
turns belief into what the swarm believes. A run's raw event log is the history
those beliefs are distilled **from**. Writing every attempt through `observe`
makes each one look like something a future branch should read, and the pool's
retrieval would start returning "branch 7 started at 12:04".

Events are history. Lessons are conclusions. One is appended by everything, the
other is blessed by a gate.

## The vocabulary

One append-only `event` table, as ADR-0091 settled, with lifecycle and
outcome-bearing facts in the **same** vocabulary. The split people reach for —
`event` for things worth counting, `trace` for noise — is wrong the moment
something needs a fact from both, and then you write the join you split to avoid.

| type | about | why it is here |
| --- | --- | --- |
| `run-started` | run | the anchor; carries goal, seed, base commit |
| `branch-spawned` | attempt | how wide the generation actually went |
| `attempt-finished` | attempt | the diff's fate, per repair |
| `gate-verdict` | attempt | the score, and what it said (ADR-0088) |
| `lesson-read` | attempt | which lessons reached the prompt — the other half of `attribute` |
| `lesson-written` | attempt | what came out |
| `capsearch-hit` / `capsearch-miss` | run | whether reuse discovery worked |
| `reuse` | artifact | a component was taken rather than rebuilt |
| `run-resolved` | run | how it ended: merged, failed, exhausted, interrupted |

`capsearch-miss` earns its place by being the most useful row here: it is the
graph naming a capability the pool lacks, which is the signal for what to build
next, and it costs one insert.

**Two fields, not events.** `started_at` and `resolved_at` live on the `run`
node. "When did this run happen" is a property of the run, not an assertion
about it, and a timeline that has to reconstruct its own bounds by scanning
events is a timeline that gets them wrong when an event is missing.

## Who writes it, and why not through a component

`comp-goalrun` writes the events itself, as SurrealQL over the `--surreal-url` it
already holds.

That looks like a violation — components reach the store through
`knowledge:graph`, which exists precisely so they can. It is not, because
`goalrun` is not a component. It is the native driver that deploys the fleet, and
[ADR-0091](0091-one-store-one-schema.md) already established this shape:
`comp-capgraph` writes the capability graph's projection straight to `/sql` for
the same reason. The rule is not "everything goes through the contract" — it is
**components go through the contract, because a component has no other way and
should not be given one.** A native driver holding the database URL as an
argument is a different thing.

The alternative was routing `goalrun` through the deployed memory app, which
would mean exposing the graph's write verbs on an HTTP surface reachable by
anything that can reach the app. That is a larger hole than the one it closes.

## What a stopped branch leaves

An `interrupted` outcome on `attempt-finished`, and nothing else. The partial
work is discarded.

Specifically **not** judged by the gate. ADR-0088 makes what a gate says the
thing the next attempt reads, so a verdict on a truncated diff is a lesson about
work nobody intended to submit, and it would be retrieved by future runs as if it
were about finished work.

Recording the interrupt at all is the point. [ADR-0082](0082-a-project-owns-a-repo-and-a-queue.md)
deferred interruption *"until the interruption rate is understood"* — and a rate
cannot be understood if it is never written down. This ADR does not build
interruption; it makes the data exist so that decision can be revisited with
something other than an impression.

## Cost

Measured in [ADR-0091](0091-one-store-one-schema.md): the graph half is free
(30x the nodes cost 1.0x the query), and the pool is a full table scan that no
index fixes. Events land in the accumulated half, so they grow the scanned set.

At the rates that matter this is nothing — a run of four branches over three
rounds with two repairs each emits on the order of a hundred events, against a
pool already measured at 200,000 lessons in 55ms. The trigger to revisit is the
same one recorded there: roughly a million rows, at which point the
`lesson -about-> interface` edge stops being a note and becomes work.

## What this does not do

No run *view* — that is the console's slice two, and it reads what this writes.
No interrupt path. No retention policy: events are append-only and nothing
prunes them, which is correct until it is not, and `decay` already exists for
the pool when that day comes.
