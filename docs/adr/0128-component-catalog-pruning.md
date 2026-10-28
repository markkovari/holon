# ADR-0128 — Capability Component Catalog Pruning and Consolidation

*With 290 components in `components/`, cognitive load, build times, and registry scanning have increased; inventorying dependencies via `comp-capgraph` and consolidating redundant adapters into cohesive domain packages keeps the catalog clean and fast.*

**Status: accepted.**

## The problem

1. **Catalog sprawl**: `components/` expanded to 290 directories. Many are single-interface wrappers, micro-variants, or temporary test stubs from early milestones.
2. **Polyglot test duplication**: Several components exist solely as multi-language reproductions (`bytes-codec`, `bytes-codec-js`, `bytes-codec-go`, `bytes-codec-py`, `bytes-codec-cs`, `bytes-codec-zig`). While valuable as polyglot proofs (docs/POLYGLOT.md), mixing them with production capability domains clutters the component tree.
3. **Build and test overhead**: `cargo xtask build` and `components/gates.sh` traverse all 290 components, increasing CI cycle times and cache footprint.

## The decision

1. **Separate Proof-of-Concept & Polyglot Reproductions**:
   - Move language verification components (`*-go`, `*-py`, `*-cs`, `*-zig`, `*-c`) to `components/polyglot/` or archive trees, keeping top-level `components/` reserved for production capabilities.
2. **Prune Stale & Unreferenced Components**:
   - Use `comp-capgraph` (ADR-0089, ADR-0097) to identify orphan components that are neither imported by any application spec nor referenced in active gates.
   - Archive dead clinic/mock experiments.
3. **Consolidate Micro-Adapters**:
   - Group related utility interfaces (e.g. string/hash/codec helpers) into coherent library components rather than creating one component directory per WIT function.

## What it does not do

- Active production domain components (`grocery-domain`, `events-domain`, `billing-domain`, `llm-router`, `vcs-store`, `park-store`) are preserved.
- WIT interfaces in `wit/` are immutable and backward-compatible (ADR-0006, witsurface tests).
