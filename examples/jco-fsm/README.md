# jco-fsm

Drives the `fsm:workflow` component in-process with [jco](https://github.com/bytecodealliance/jco).

`fsm:workflow` is a **declarative state machine**: you `define` the states and
the legal transitions between them once, then spin up instances and drive each
through those transitions. Every `fire` is validated against the definition — an
event that isn't legal from the current state is rejected, never silently
dropped — and each accepted transition is recorded in an append-only `history`.

This example models the vet-clinic **appointment lifecycle**:

```
booked ──confirm──▶ confirmed ──complete──▶ completed (terminal)
   │                    │
   └──────cancel────────┴───────────────────▶ cancelled (terminal)
```

The component imports `wasi:keyvalue/store` for persistence; here it is backed
by a trivial in-memory `Map` (`src/keyvalue-shim.js`). That shim is swappable —
point it at redis/sqlite/NATS and the component is unchanged. `wasi:clocks` is
auto-shimmed by jco.

## Run

```bash
npm install
npm test
```

`npm test` transpiles `fsm_workflow.wasm` into `gen/` (mapping the keyvalue
import to the shim) and runs the test suite via `tsx --test`.
