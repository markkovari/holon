# Holon

**A library of WebAssembly capability components, and four ways to run what you
compose from them.**

The boring infrastructure every backend reimplements — sessions, rate limits,
search, money, validation, idempotency, audit, secrets, a virtual git, a
knowledge graph — as **197 `wasm32-wasip2` components**, each a WIT contract plus
a reference implementation. Compose them into an app with `wac`, then deliver that
app to a VPS, a fleet, or somebody else's wasmCloud, from one spec.

The name is the design. A **holon** is a whole that is also a part of a larger
whole (Koestler's term; *holacracy* descends from it). A component is exactly
that: a complete thing behind its own contract, and a part of the app it is
composed into. The graph nests, measured to depth 4.

## The two halves

**The library.** `records:store`, `auth:identity`, `event:bus`, `sched:timer` and
ninety more interfaces, each with a provider and consumers. Contracts are WIT, so
a component is language-agnostic and its boundary is enforced by the linker
rather than by review. `docs/CAPABILITY-GRAPH.md` is derived from the *built
artifacts* — a component's real imports are in its binary, so it cannot drift.

**Delivery.** One hand-authored `apps/<name>.toml`, four backends. Moving between
them is an edit and a different recipe, never a rewrite.

| lane | what runs it | for |
|---|---|---|
| **one box** | `comp-host` + systemd + Caddy | your own machine, no control plane |
| **a lattice** | `comp-host` per node + reconciler + ingress over NATS | several boxes, where *which* box stops being your decision |
| **wasmCloud 1.x** | wadm, over NATS | somebody else's wasmCloud |
| **wasmCloud 2.x** | the runtime-operator, over the Kubernetes API | current wasmCloud — no wadm, no OAM |

```bash
just compose-gate                    # components -> one .wasm
just selfhost-render gate            # read the unit and route before trusting them
just selfhost-deploy gate my-vps     # ship it
```

HTTP is not the only trigger. `sched:timer`, `event:bus` and `cron:expr` are
pull-based by design — which keeps them pure WASI and portable — so `comp-relay`
drives them in every lane, and an app gains a trigger by declaring `[triggers]`
rather than by changing its exported WIT.

See [`docs/SELFHOST.md`](docs/SELFHOST.md) for all four lanes and what each costs.

## Why WebAssembly, and what is native on purpose

Components are the unit of **isolation** — a store name resolves through host-side
state the guest cannot write, so the boundary is a linker boundary rather than a
policy ([ADR-0023](docs/adr/0023-isolation-is-a-linker-boundary.md)) — and the unit
of **composition**: an app is a root component plus everything `wac` pulls in
behind it, derived from its own imports rather than hand-written
([ADR-0087](docs/adr/0087-a-composition-is-derived-not-written.md)).

Only what *must* be native is native, and
[ADR-0095](docs/adr/0095-what-is-allowed-to-be-native.md) is the written test for
admitting anything new: it must need a capability WASI denies a guest — a process,
a raw socket, a held subscription — not a convenience. The host that runs the wasm
(`host/`, wasmtime 45), the reconciler that holds a NATS subscription and a timer
(`reconciler/`), the gate that spawns processes, the relay that holds a clock, and
the CLI. Everything else is a component.

## The agentic loop — **paused, and kept**

Holon also contains an engineering loop that runs AI agents in parallel over this
substrate: each branch explores the same goal in its own environment, a real test
gate judges every candidate, and a selector opens the winner as a pull request.

```
goal ──┬─▶ branch 0 (its own env) ─▶ writes code ─▶ real tests ─▶ 1000
       ├─▶ branch 1 ─▶ … ─▶  500          ↑                        │ selector
       ├─▶ branch 2 ─▶ … ─▶ 1000    the gate runs the             ▼ picks a winner
       └─▶ branch 3 ─▶ … ─▶  666    project's own commands   ─▶  pull request
```

It works, and it is not what this repository is currently being pushed on.

**What it proved.** Nothing is mocked on the path to a landed change: a real model
writes the code (`components/anthropic-provider`), a real suite gates it
(`components/checks-runner` over a native runner), a real forge opens the PR
(`components/github-forge`). A goal to `slugify` a string went from a queue to a
merge-ready pull request for a few cents, and a two-part goal ran **on this
repository** — a component and the probe that calls it, built by separate branches
against a shared contract, joined and landed as one PR.

**Why it is on hold.** The machinery is not the limit; the gate is. **A run is
exactly as good as the checks a person wrote for it.** A gate that already passes
on the base tree accepts anything, and the first real decomposed run scored a
perfect 1000 on two candidates that had deleted their own component exports.
Criticising the gate is [`.comp/goals/07-nothing-criticises-a-gate.md`](.comp/goals/07-nothing-criticises-a-gate.md),
and until something does, more search buys less than better contracts and a way to
ship them.

Nothing is deleted: the components, the ADRs
([0078](docs/adr/0078-an-environment-is-a-derived-app.md)–[0094](docs/adr/0094-a-capability-describes-itself-in-a-callers-words.md)),
the traces and `goal-demo.sh` all still run. Resume it when a gate can be
criticised.

```bash
# CHECKOUT is the repository the loop works ON — not this one.
CHECKOUT=~/src/widgets REPO=acme/widgets bash goal-demo.sh real     # goal → PR
```

## Where to look

**The library**

| | |
|---|---|
| what is using what, and may I change it | [`docs/CAPABILITY-GRAPH.md`](docs/CAPABILITY-GRAPH.md) — 93 interfaces, 422 import edges, and the 68 apps composed from them; `record-store` is inside 38 of them |
| the fifty-three showcase apps, one file each | [`docs/apps/`](docs/apps/README.md) |
| every other document, and which are generated | [`docs/README.md`](docs/README.md) |
| the original capability-library README (archived) | [`docs/archive/capability-library-readme.md`](docs/archive/capability-library-readme.md) |
| why a component is worth more than a note about one | [ADR-0089](docs/adr/0089-capability-accumulation.md) — 203 components, reuse enforced by a gate that reads what a candidate actually called |
| how a composition is derived rather than written | [ADR-0087](docs/adr/0087-a-composition-is-derived-not-written.md) — `reconciler/src/plug.rs` reads a component's imports out of the binary |

**Delivery**

| | |
|---|---|
| the four lanes, and what each costs | [`docs/SELFHOST.md`](docs/SELFHOST.md) |
| the app spec — the only file you write | `apps/<name>.toml`, rendered by `holon node render` |
| the lattice: nodes, a reconciler, an ingress | `fleet.example.toml`, `holon fleet render` |
| triggers that are not HTTP | [ADR-0096](docs/adr/0096-a-pull-contract-needs-a-relay.md) — `comp-relay`, and why push never replaces the sweep |
| what may be native at all | [ADR-0095](docs/adr/0095-what-is-allowed-to-be-native.md) — three questions, and the answer to the third is usually "here is the WIT" |
| what runs today, measured, and honestly missing | [`docs/CURRENT.md`](docs/CURRENT.md) |
| what a hardening sweep found, fixed, and left | [`docs/HARDENING.md`](docs/HARDENING.md) — five failures that wore the return type of a success |

**The reasoning**

| | |
|---|---|
| 96 decisions, 10 of them superseded and kept | [`docs/adr/`](docs/adr/) |
| the one rule everything else applies | [`docs/CURRENT.md`](docs/CURRENT.md#the-one-rule-everything-else-is-an-application-of) |

**The paused loop** — still runs, not the current focus

| | |
|---|---|
| how a run succeeds, and every way it fails | [`docs/SCENARIOS.md`](docs/SCENARIOS.md) |
| the agentic core | `components/{agent-writer,agent-driver,graph-selector}`, `reconciler/src/generation.rs`, `reconciler/src/bin/goalrun.rs` |
| what the swarm remembers | `components/knowledge-memory`, [ADR-0084](docs/adr/0084-two-retrievers-and-an-optimistic-database.md) |
| how two halves of one goal agree | `components/contract-registry`, `reconciler/src/compose.rs`, [ADR-0086](docs/adr/0086-parts-negotiate-a-contract.md) |
| the worklist — goals a person has written | [`.comp/goals/`](.comp/goals/) |
| the browser surface: author a goal, read a run as a graph | [`docs/apps/CONSOLE.md`](docs/apps/CONSOLE.md) — `just host-console` |

## Status

**The library is the mature half.** 197 components, 93 interfaces with a provider
and at least one consumer, 67 applications composed from them, heavily tested
across four workspaces. The capability graph is derived from built artifacts, so
it says what the code does rather than what a list claims.

**Delivery is newly complete and newly proven.** All four lanes were verified
against real infrastructure rather than asserted — the wasmCloud lanes against a
live wadm 0.21 + wasmCloud 1.6.0 and a runtime-operator 2.8.0 + wash 2.8.0 stack.
The honest edge is that a wasmCloud 2.x release host provides standard WASI plus
`wasmcloud:messaging` and nothing else, so an app importing a `comp:` interface
runs on the first two lanes and is refused at render time on the fourth, with the
reason.

**The loop is paused, not abandoned** — see above. The blocker is that nothing
criticises a gate.

The internal name is still `comp` in the code — renaming it to `holon` is itself a
goal on the worklist ([`.comp/goals/05-become-holon.md`](.comp/goals/05-become-holon.md)).
