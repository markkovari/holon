# Holon

**A goal goes in. Pull requests come out.** Holon runs AI agents in parallel over
a WebAssembly substrate: each explores the same goal in an isolated environment,
a real test gate judges every candidate, and the winner is opened as a pull
request. A person writes the goal and the checks; the machine does the rest.

The name is the design. A **holon** is a whole that is also a part of a larger
whole (Koestler's term; *holacracy* descends from it). Here it is literal on
every level:

- **each branch is a holon** — a self-contained agent handed a goal, left to
  pursue it autonomously, answerable only to the gate;
- **the environments form a holarchy** — they nest, each a complete graph that is
  part of a larger one (measured to depth 4);
- **one holon is chosen** — a selector compares the branches on what real checks
  actually found and lands the winner.

## The loop, and that it is real

```
goal ──┬─▶ branch 0 (its own env) ─▶ writes code ─▶ real tests ─▶ 1000
       ├─▶ branch 1 ─▶ … ─▶  500          ↑                        │ selector
       ├─▶ branch 2 ─▶ … ─▶ 1000    the gate runs the             ▼ picks a winner
       └─▶ branch 3 ─▶ … ─▶  666    project's own commands   ─▶  pull request
```

Nothing is mocked on the path to a landed change: a real language model writes
the code (`components/anthropic-provider`), a real test suite gates it
(`components/checks-runner` over a native runner), a real forge opens the PR
(`components/github-forge`). It has been run end to end — a goal to `slugify` a
string went from a queue to a merge-ready pull request for a few cents, and a
two-part goal ran **on this repository**: a component and the probe that calls it,
built by separate branches against a shared contract, joined and landed as one PR.

```bash
bash goal-demo.sh real      # one command: goal → PR
```

## Why WebAssembly, and what is native on purpose

Every **workload** is a wasm32-wasip2 component with a WIT contract — the agent,
the driver's loop, the gate evaluator, the selector, the forge, the model
provider. Components are the unit of isolation (a branch's store is named after
its derived app, so isolation is a linker boundary, not a policy) and the unit of
composition (a graph is components linked over wrpc).

Only what *must* be native is native: the host that runs the wasm (`host/`,
wasmtime 45), the reconciler that holds a NATS subscription and a timer
(`reconciler/`), and the CLI. A wasm module cannot spawn a process or hold a
subscription, so the boring, dangerous parts live where the operating system is —
and stay small, because the thing that enforces every boundary is the thing you
least want to rebuild often.

## The substrate it grew from

Holon began as a **library of 40+ WASI capability components** — the boring
infrastructure every backend reimplements (sessions, rate limits, search, money,
validation, idempotency, audit, secrets, a virtual git, a knowledge graph), each
a WIT contract plus a reference implementation. That library is still here under
`components/`, and it is what the agentic loop is built out of: `vgit:store` is
git in blob storage, `artifact:cache` is single-flight derived work, `blob:store`
and `comp:store` are the content-addressed and compare-and-set halves of state.
The engineering loop is not bolted on top of the platform; it *is* the platform,
pointed at itself. (The original capability-library README is kept at
[`docs/README-capability-library.md`](docs/README-capability-library.md).)

## Where to look

| | |
|---|---|
| what runs today, measured, and honestly missing | [`docs/CURRENT.md`](docs/CURRENT.md) |
| the thirty-two showcase apps, one file each | [`docs/apps/`](docs/apps/README.md) |
| how a run succeeds, and every way it fails | [`docs/SCENARIOS.md`](docs/SCENARIOS.md) |
| the reasoning — 77 decisions in force, 8 superseded and kept | [`docs/adr/`](docs/adr/) |
| the worklist — goals a person has written | [`.comp/goals/`](.comp/goals/) |
| the agentic core | `components/{agent-writer,agent-driver,graph-selector}`, `reconciler/src/generation.rs`, `reconciler/src/bin/goalrun.rs` |
| what the swarm remembers | `components/knowledge-memory`, [ADR-0084](docs/adr/0084-two-retrievers-and-an-optimistic-database.md) |
| why a component is worth more than a note about one | [ADR-0089](docs/adr/0089-capability-accumulation.md) — 150 components, reuse enforced by a gate that reads what a candidate actually called |
| how two halves of one goal agree | `components/contract-registry`, `reconciler/src/compose.rs`, [ADR-0086](docs/adr/0086-parts-negotiate-a-contract.md) |

## Status

The engine works end to end and is heavily tested (~400 tests across four
workspaces). It searches, it remembers what it evaluated, and two subgraphs can
negotiate an interface neither has written yet.

The honest edge is one level up from the machinery: **a run is exactly as good as
the checks a person wrote for it.** A gate that already passes on the base tree
accepts anything, and the first real decomposed run scored a perfect 1000 on two
candidates that had deleted their own component exports. Criticising the gate is
goal 07, and it is the next thing worth building.

The internal name is still `comp` in the code — renaming it to
`holon` is itself a goal on the worklist
([`.comp/goals/05-become-holon.md`](.comp/goals/05-become-holon.md)), and the
honest first thing to hand the loop once it can safely make a multi-file change.
