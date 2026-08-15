# A branch reads what the swarm learned — ✅ done

> **Built:** a failed branch writes what it failed on (the gate's own words, no
> model in the path), each branch of a run reads a different slice of the pool,
> and what a branch read is attributed to its verdict.
> `reconciler/tests/learning.rs` proves the loop closes with no AI calls at all:
> run one fails and writes, run two reads and passes, and a branch that reads
> nothing still fails — which is what makes the middle claim mean anything.
>
> **Slice 3 too:** a candidate that passed is distilled by one cheap call into at
> most 900 characters of transferable advice and promoted to `patterns` — through
> the `promotion` interface an agent's world does not contain, and refused outright
> on a score that did not pass. `NOTHING` is a first-class answer, because most
> passing candidates teach nobody anything and a pool that records a platitude for
> each of them buries the few that matter.
>
> **Retention too:** every run sweeps the pool on its way out — entries nothing has
> read twice and nothing has touched in `--forget-after-days` (30 by default) are
> deleted. Driven by the run rather than by a daemon, because a `decay` that is
> exposed and never called is the gap ADR-0081 caught in alpha-swarm2 and naming it
> does not close it.
>
> **And the decomposed path too:** each PART reads on its own goal — a backend and
> a frontend attempting one feature want different lessons — writes what it failed
> on, and has its reading attributed to its own verdict. Asserted in
> `reconciler/tests/compose.rs`: the frontend fails twice before the contract moves,
> and the pool must hold a lesson naming the check it failed. Wiring a pool in and
> having it quietly do nothing is the failure this session kept finding, so the
> test looks rather than assumes.

**Traces to:** ADR-0084 (built: the pool, the policy, the retrieval, the work
dedup) and `docs/CURRENT.md` — *"no branch yet READS a lesson"*.

## Where it stands

Built and demonstrated through a real host: `knowledge:memory` decides who may
write what the swarm believes, fuses KNN with TF-IDF, weights what it returns by
what happened to the runs that read it, and `holon goal run --surreal-url …` asks
`already-done` before spending a generation and records every branch's verdict
after. The composed e2e watches all of it work.

What is missing is the half that makes the loop cleverer rather than cheaper.

**Why human-led:** slice 1 puts model-visible text into every branch's prompt from
the spawn path, which is the same surface goal 03 marks human-led for the same
reason — what a branch is shown decides what it can do, and a pool a tenant could
write is a pool a tenant could use.

## Three slices, in order

1. **Read.** `plan_for` puts a lens on each branch's goal; it should also put
   retrieved lessons there, with `recall`'s `k`, `budget` and `pools` varied per
   branch — the diversity budget already argued for in ADR-0081. One branch per
   generation reads NOTHING; that branch already exists (`reads_prior: false`) and
   is the control arm.
2. **Attribute.** A branch's verdict must name the handles it read, or the
   weighting has nothing to weight. `Entry` needs the handles it was given.
3. **Promote.** A verified diff distilled by one cheap call into ≤900 characters,
   written to `patterns` by the post-gate hook — the only writer that may. Until
   this exists `patterns` stays empty and retrieval runs on `solutions` + `errors`.

## What to watch, not just build

- **Herding.** Every branch reading the same top-k is an expensive way to run one
  branch. Report DISTINCT retrieved sets per generation, as loudly as the selector
  reports distinct candidates.
- **The control arm.** Without a branch that reads nothing there is no way to say
  whether the pool helped.
- **Decay.** Nothing prunes. ADR-0081 caught alpha-swarm2 exposing `decay` and
  never scheduling it; this repo should schedule it or not claim it.
