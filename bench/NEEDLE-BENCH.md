# needle vs querying the graph

*Measured before building, because the interesting answer was the cheap one.*

The console shows a queue of goals as a graph. The proposal was a command box on
top of it, driven by [Needle 2](https://github.com/cactus-compute/needle) — a 45M
tool-calling model, 14MB, grammar-constrained JSON out, with a calibrated
confidence head. It is built for exactly this shape of job, and it is the only
place in Holon a model that small could plausibly earn a seat.

The alternative was to query the graph directly: match the typed words against
the five lifecycle states and the goal titles already on the screen. About thirty
lines, no dependency, no server.

## Result

| | correct | coverage | latency | dependency |
|---|---|---|---|---|
| **querying the graph** | **24/25** | 25/25 answered | 0.03 ms | none |
| Needle 2 | 11/25 | 25/25 answered | 33 ms median | a Python service, 48 MB RSS |
| Needle 2, gated at confidence ≥ 0.7 | 8/10 | 10/25 answered | " | " |

Twenty-five queries written from the vocabulary the UI actually uses, before
either implementation existed: twelve filters by lifecycle state, six that name
one goal, three that ask what happened in a run, four that must be refused.
`bench/needle/cases.json`.

```bash
node bench/needle/baseline.mjs          # scores the console's own query.ts
./nv/bin/python bench/needle/needle_bench.py
```

The baseline scores the **shipped file** — `examples/console/ui/src/query.ts`,
type-stripped by node — not a copy of it, so the thing measured cannot drift from
the thing served.

## Why the small model loses, and it is not that it is small

Every state is one of five words, and **every goal title is on the screen when
the person types**. They are picking from a visible list, not naming something
unseen. Matching wins because the answer is already in front of you; inference is
solving a problem the UI already solved.

Where Needle actually failed:

- **World knowledge it does not have.** `stuff that blew up` → `running`.
  `in flight` → `failed`. `not started yet` → `running`. Mapping colloquial
  English onto a domain vocabulary is the one thing a 45M model cannot do, and
  it is most of what a command box is asked to do. The matcher gets these from a
  six-word synonym list per state.
- **Titles it can see and still misses.** `diversity` → a state filter, with
  `Diversity beyond seed` sitting in its enum. `open the latest run of diversity`
  → the wrong goal entirely.
- **No refusals.** `what's the weather in Lagos` → `open_run`. `write me a haiku`
  → `open_run`. The docs promise an empty call for off-topic input; on this
  toolset it never came. Confidence caught both (0.006, 0.002) — the calibration
  is real even when the answer is not.

## What the confidence head is worth

It is the best thing in the result. Gating at 0.7 lifts precision from 44% to
80% by declining 15 of 25 queries — the model reliably knows when it does not
know. But 80% of 40% is still far below 96% of 100%, so the gate cannot rescue
this; it can only make the failure quiet.

One high-confidence mistake matters more than the rest: `start drive the queue`
came back as a call at **0.93**. Nothing was wired to `start`, so nothing
happened. Had the toolset included the actions a person can take from that panel,
a typed sentence would have spent money and opened a pull request — which is
exactly why ADR-0082 keeps starting a goal a deliberate act, and why the shipped
box refuses that phrase outright rather than parsing it.

## What was NOT tested, and would change the answer

- **Fine-tuning.** Needle's pitch is LoRA on your own tools; this is the base
  model. `needle generate-data` off these three schemas plus a few hundred
  samples is the obvious next move, and the honest reading of 11/25 is "the base
  model does not do this", not "the model cannot be made to".
- **A bigger catalogue.** Five states and five visible titles is the matcher's
  best case. At fifty tools, with the retrieval head rendering the top five, the
  curves are not obviously in the same order — the matcher's synonym list is the
  thing that stops scaling first.
- **Anything the user cannot see.** "goals about caching that failed last week"
  is a query with no words on the screen to match. That is where a model earns
  its seat, and none of the twenty-five cases are it, because the console does not
  show that data yet.

## Decision

Ship the matcher. Do not add the dependency.

Revisit if the catalogue grows past what a synonym list can cover, or if a tuned
`.cact` beats 24/25 on this same file — which is the point of leaving the cases,
the runner and the numbers here rather than a paragraph saying it was tried.
