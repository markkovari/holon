# golem-provider — a native wRPC→Golem durable-worker capability provider

The first thing in this repo that is **not a wasm component**. Everything so far
is components (compiled to wasm, `wac`-composed) running on a host. This is a
**native wasmCloud v2 capability provider** — a Rust binary that joins the NATS
lattice, serves a typed WIT interface over **wRPC**, and bridges each call to a
**durable [Golem](https://www.golem.cloud/) worker**. A consumer component calls
a normal imported function; behind the contract, the work runs as a
crash-proof, resumable Golem workflow.

## Why this is the axis worth adding

- **New artifact class.** A *provider* is host-side native code that satisfies a
  component's import over the lattice — the other half of the Component Model the
  repo has never shown. It's how you reach capabilities that can't be a
  sandboxed wasm module: a database driver, a GPU, or here, an external durable
  runtime.
- **It's the payoff `saga` was waiting for.** SAGA.md's legs are in-process
  simulated bookings; SAGA-BENCH notes "outbox is the upgrade for a real remote
  leg." This is that upgrade, but stronger: a leg becomes a **durable Golem
  worker** that survives its *own* crashes, retries with exactly-once effects,
  and resumes — while the saga still owns compensation. Two durability models
  compose: the saga's (persisted state + rollback) and Golem's (durable
  execution of a single step).
- **Contract-first, as always.** The consumer imports `durable:workflow/orchestrator`
  and never learns it's Golem. Swap the provider for a Temporal or Step-Functions
  bridge and the component is unchanged — the same "the WIT is the product" claim,
  now on the provider side.

## The contract (`durable:workflow@0.1.0`)

A consumer component imports this; the provider exports it. (Repo-consistent
naming of the ROADMAP brief's `local:workflow/orchestrator`.)

```wit
package durable:workflow@0.1.0;

interface orchestrator {
    record run-request { workflow-id: string, payload: string }   // payload = JSON
    variant run-error { not-found(string), invalid-input(string),
                        worker-failed(string), unavailable(string) }

    // Rung 1 — blocking (matches the ROADMAP brief): trigger a worker, wait,
    // return its result. Simple, but ties up the caller for the worker's life.
    trigger: func(req: run-request) -> result<string, run-error>;

    // Rung 2 — the honest durable shape: fire-and-poll, so a long-running or
    // crash-recovering worker doesn't hold the request open.
    record run-status { state: string /* pending|running|completed|failed */,
                        output: option<string> }
    start:  func(req: run-request) -> result<string, run-error>;   // -> run-id
    status: func(run-id: string) -> result<run-status, run-error>;
}
```

## Control flow

```
wasm component            NATS lattice          this provider (native)         Golem
──────────────            ────────────          ──────────────────────         ─────
orchestrator.trigger(req) ── wRPC ─────────────▶ wit-bindgen-wrpc handler
                                                 map RunRequest → golem_wasm_rpc::Value
                                                 golem_client WorkerClient.invoke ──────▶ durable worker runs
                                                 map Golem Value → String  ◀──────────── result (exactly-once)
◀───────────── result<string> ── wRPC ─────────  return
```

The provider uses: `wasmcloud-provider-sdk` (host handshake + linking),
`wit-bindgen-wrpc` (generate the async server trait from the WIT),
`wrpc-transport-nats` (serve it on the lattice), `golem-client` (invoke workers),
`golem-wasm-rpc` with the `host` feature (Rust structs ↔ `Value`), tokio, tracing.
`GOLEM_URL` + `GOLEM_TEMPLATE_ID` come from provider config at link time.

## The pieces

| piece | what | kind |
|---|---|---|
| `providers/golem-workflow` | the wRPC→Golem provider | **native Rust binary** (new) |
| `wit/durable-workflow.wit` | the `durable:workflow/orchestrator` contract | WIT (new) |
| `components/workflow-caller` | a tiny consumer: HTTP `POST /run {workflowId,payload}` → `orchestrator.trigger` | wasm component (new) |
| `infra/golem-wadm.yaml` | OAM app: link the caller component to the provider | wadm manifest (new) |
| a Golem workflow | e.g. `book-flight` — the durable worker the provider invokes | Golem app (Rust/wasm) |

## What's repo-side vs what needs live infra (the honest ceiling)

Unlike saga/pulse — fully e2e'd on the lightweight `host/` wasmtime binary — a
**provider cannot run on `host/`**. It's a lattice participant. So:

- **Repo-side (compiles, reviewable, unit-testable):** the provider crate, the
  WIT, the consumer component, the wadm manifest, and the type-mapping logic
  (RunRequest ↔ `golem_wasm_rpc::Value`) — which *is* unit-testable in isolation
  and is where the real bugs live.
- **Needs live infra (a documented `just` recipe, not a `cargo test`):** the true
  end-to-end run needs **NATS + a wasmCloud v2 host (`wash`) + a Golem instance**
  (Golem OSS via docker) with a deployed `book-flight` worker. That's heavier
  than anything else here and lands in "explicit-ask cluster" territory.
- **Known risk:** the wasmCloud-v2 + `wrpc` + `golem-*` crate matrix is
  version-sensitive (the ROADMAP brief flags it). Rung 1 is partly a
  dependency-resolution exercise; budget for it.

## Build order + what's verified

1. ✅ **Contract + type mapping** — `durable:workflow.wit` + the `RunRequest →
   Value` / result mapping as a unit-tested library (6 tests, against the real
   `golem_wasm_rpc::Value`).
2. ✅ **The provider compiles** — `providers/golem-workflow`: `wit-bindgen-wrpc`
   server trait, `wasmcloud-provider-sdk` handshake, the `Handler` +
   `serve_provider_exports` wiring. A real native provider binary.
3. ✅ **Live Golem e2e (verified)** — Golem 1.5 running locally (`golem server
   run`), a real agent deployed, and the provider's bridge call (`invoke_golem`)
   invoked against it, twice, asserting the durable count advances. Automated:
   `GOLEM_E2E=1 cargo test` (see the crate README).
4. ◻︎ **Saga integration** (follow-up) — a `saga` variant whose legs call
   `orchestrator.trigger` so each leg is a durable Golem worker. Designed, not
   yet built.

## Status & the honest boundary

What is **live-verified**: our typed contract → the provider's Golem bridge → a
**real, stateful Golem 1.5 durable worker** → result. Proven by an automated
test hitting a running Golem.

What is **not run live here** (and why): the *front* half — a wasm component
reaching the provider over wRPC on a wasmCloud lattice. The provider **compiles**
with the correct provider-SDK/wRPC structure, but the installed `wash` (2.3, the
new component-first "Wasm Shell") has no classic-lattice host/`par`/wadm to run
and link a native provider. Running that half needs the classic wasmCloud host —
future work, not fake-able.

Two findings worth keeping:

- **wRPC alignment.** `wasmcloud-provider-sdk 0.17.1` pins `wrpc-transport 0.28`;
  `wit-bindgen-wrpc 0.11` pins `0.29` — mismatch → `WrpcClient: Serve` unsatisfied.
  **Fix: pin `wit-bindgen-wrpc 0.10`** (also `0.28`) so the tree unifies.
- **Not `golem-client`.** The ROADMAP brief suggested it, but 1.3 is build-time
  code-generated and under a non-OSS license; Golem 1.5 agents also expose plain
  HTTP endpoints via the gateway. So the bridge is a direct `reqwest` POST —
  MIT-clean, controllable, and enough for the endpoint-style agents.

## Non-goals (v1)

Golem worker *authoring* beyond a trivial `book-flight` (that's a Golem tutorial,
not this bridge), streaming/partial results, and a k8s deploy of the whole stack
(local docker lattice first). A generic "call any external durable runtime"
abstraction is the eventual contract; this bridges one (Golem) concretely first,
the way `vet-domain` predated the extracted capabilities.
```
