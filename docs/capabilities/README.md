# Capabilities

A capability is a WIT package that **exports** one contract and **imports** only
generic WASI. The backend is bound at compose/deploy time, never in the WIT — which
is why the same component runs over an in-memory map, NATS, redis or sqlite
unchanged.

**The list of them is generated, not written here:**

| for | read |
|---|---|
| every component, its package, deps, config knobs, size, reusability | [`../../components/CATALOG.md`](../../components/CATALOG.md) |
| what a component *really* imports, read out of the built wasm | [`../CAPABILITY-GRAPH.md`](../CAPABILITY-GRAPH.md) |
| what a capability is *for*, in a caller's words | its own `wit/*.wit` doc comments — enforced by a lint ([ADR-0094](../adr/0094-a-capability-describes-itself-in-a-callers-words.md)) |

A hand-maintained table here would be a fourth, differently-wrong copy. The two
above are derived from the tree.

## The ones that needed prose

Three capabilities have more to say than a table row:

| doc | what it covers |
|---|---|
| [`USAGE.md`](USAGE.md) | consuming `auth:identity` — sessions, RBAC, introspection — from your own component |
| [`CRDT.md`](CRDT.md) | `crdt:merge`, the convergence primitive `scribe` is built on: many replicas, no lock, out-of-order exchange |
| [`GOLEM.md`](GOLEM.md) | `golem-workflow`, the first thing here that is **not** a wasm component — a native wRPC capability provider over Golem durable workers |

## The rules a capability lives under

Stated once, in the decision that made them, rather than restated here:

- **reuse before build** — [ADR-0089](../adr/0089-capability-accumulation.md)
- **a composition is derived from imports, not typed by hand** — [ADR-0087](../adr/0087-a-composition-is-derived-not-written.md)
- **does this plug fit?** the real subtype check — [ADR-0048](../adr/0048-does-this-plug-fit.md)
- **what is allowed to be native** — [ADR-0095](../adr/0095-what-is-allowed-to-be-native.md)
