# jco-quota

Runs the `quota:meter` WebAssembly component in-process under Node via
[`jco`](https://github.com/bytecodealliance/jco) transpilation.

## What it does

`quota:meter` tracks **cumulative** consumption against a per-subject limit
within a fixed period. Unlike a sliding-window rate-limiter (which expires
events continuously), a quota meter accumulates a running total and resets only
when the period rolls or `reset` is called.

- `reserve(subject, amount, limit, periodSeconds)` — conditional: bumps usage by
  `amount` only if it stays within `limit`. Otherwise throws `exceeded` whose
  payload `val` carries the remaining balance.
- `recordUsage(subject, amount, limit, periodSeconds)` — unconditional: always
  adds `amount`, even past the limit (remaining floors at 0).
- `peek(subject, limit, periodSeconds)` — read the current balance.
- `reset(subject)` — clear accumulated usage. Note: `reset` takes no period
  argument and clears only the component's **default** window (30 days =
  2592000s), so to observe it you must meter against that same period.

The running total is bumped with an **atomic increment** (`wasi:keyvalue/atomics`),
so two concurrent `reserve` calls can't both read the old balance and oversell
the limit.

## Host shims

The component imports `wasi:keyvalue/store`, `wasi:keyvalue/atomics`, and
`wasi:clocks`. `jco` auto-shims clocks; the keyvalue store and atomics are
supplied here as trivial in-memory maps:

- `src/keyvalue-shim.js` — in-memory bucket store (exports `buckets`).
- `src/atomics-shim.js` — atomic integer increment over the **same** store.

Both are swappable: point the `--map` flags at redis/sqlite/NATS-backed modules
implementing the same WIT interface and the component is none the wiser.

## Run

```bash
npm install
npm test        # transpiles quota.wasm -> gen/, then runs the node:test suite
```
