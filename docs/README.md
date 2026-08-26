# The documentation

Four kinds of thing live here, and mixing them up is what made this directory hard
to read: **generated** files (never edit), **current** descriptions of what runs,
**decisions** with their measurements, and **archived** documents kept for their
reasoning rather than their accuracy.

## Start here, by what you are doing

| you want to | read |
|---|---|
| know **what runs today**, and what is honestly missing | [`CURRENT.md`](CURRENT.md) |
| know **why any of this exists**, with the numbers | [`WHY.md`](WHY.md) |
| **find a component** — its package, deps, size, whether it is reusable | [`../components/CATALOG.md`](../components/CATALOG.md) *(generated)* |
| see **what a component really imports**, from the built wasm | [`CAPABILITY-GRAPH.md`](CAPABILITY-GRAPH.md) *(generated)* |
| **run an app** on your own machines | [`SELFHOST.md`](SELFHOST.md) |
| see **an app that works** — one file each | [`apps/`](apps/) |
| **consume a capability** from your own component | [`capabilities/`](capabilities/) |
| know **why a design is the way it is** | [`adr/`](adr/) — every decision, with reading paths |
| know **what a graph run succeeds or fails at** | [`SCENARIOS.md`](SCENARIOS.md) |
| know **what a security sweep found** | [`HARDENING.md`](HARDENING.md) |

## What is generated

Do not hand-edit these; the edit is lost on the next regenerate, and the guard
tests notice.

| file | from | regenerate |
|---|---|---|
| [`../components/CATALOG.md`](../components/CATALOG.md), `catalog.json` | every `components/*/` | `python3 tools/gen-catalog.py` |
| [`CAPABILITY-GRAPH.md`](CAPABILITY-GRAPH.md) | the **built** artifacts' real imports | `just capgraph` |
| [`apps/*.md`](apps/) headers | app specs | `python3 tools/gen-app-specs.py` |

## Historical, and kept on purpose

Nothing here is deleted — ADR-0001's rule is *supersede rather than edit*, and it
applies to the design docs too. These describe a world that no longer runs, and are
kept because the reasoning is still worth reading:

| doc | what it was | what replaced it |
|---|---|---|
| [`PLATFORM.md`](PLATFORM.md) | the original five-phase narrative plan for a wasmCloud-hosted multi-tenant PaaS | its central isolation bet was falsified, then won by owning the host — [ADR-0023](adr/0023-isolation-is-a-linker-boundary.md). Where it disagrees with an ADR, the ADR wins |
| [`archive/capability-library-readme.md`](archive/capability-library-readme.md) | the repository's first README: one contract, 40 components, the wasmCloud 1.x/2.x lanes | [`../components/CATALOG.md`](../components/CATALOG.md) for the list, [ADR-0021](adr/0021-there-is-no-kubernetes.md) for the lane |
| [`adr/`](adr/) — the superseded ones | see the *History* table at the end of [`adr/README.md`](adr/README.md) | each names its own replacement |

## The rest

- [`measure/`](measure/) — one-off experiments with a question and an answer, not features.
- [`media/`](media/) — the GIFs the app docs embed.
