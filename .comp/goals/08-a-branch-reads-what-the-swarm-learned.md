# A branch reads what the swarm learned — 🔴 human-led

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
