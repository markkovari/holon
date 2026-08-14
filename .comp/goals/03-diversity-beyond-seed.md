# Diversity beyond the seed — 🔴 human-led

**Traces to:** `docs/SCENARIOS.md` — *"branches differ only by seed … herding is
unmitigated and does not announce itself"*, and `docs/CURRENT.md` — *"an
environment is a COPY, so no branch runs a different MODEL from its siblings."*

## What is wanted

Two branches that explore the same goal should be able to differ by more than a
number. Today they differ by a lens on the prompt (`generation::Strategy`) and a
seed; the environments that back them are exact copies of their parent, so a
branch cannot run a cheaper model, cannot keep an artifact a sibling could reuse,
and cannot read a lesson from a run that came before it.

Three threads, each its own goal once this one is split:

1. **A per-branch overlay on spawn.** An environment is a derived app (ADR-0078);
   let the derivation take a small overlay — a different component or config for
   one branch — so a generation can put haiku on three branches and opus on the
   fourth and compare what the money bought.
2. **Artifacts handed between branches, not rebuilt.** `artifact:cache` exists
   and dedupes derived work; wire the driver to it so a compile or an index a
   sibling produced is looked up, not recomputed.
3. **Knowledge that improves.** `knowledge:graph` stores and traverses; nothing
   promotes a lesson by outcome or decays a wrong one. Weight what is retrieved
   by whether the branch that wrote it was accepted.

Herding should also *announce itself in the run's summary*, not only in the
selector's `distinct` count — a generation whose branches converged bought
nothing, and that should be as loud as a failure.

## Why it is human-led

Every thread touches the spawn path, the driver's loop, and a stored contract,
and thread 3 needs an embedding provider that is not wired (retrieval is lexical
only today). These are design decisions with security edges — an overlay a tenant
could author is an overlay a tenant could use to escape its box (ADR-0008). A
person leads; the agent may help on the pure pieces once they are carved out.
