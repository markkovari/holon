# Roadmap — the showcase worklist

> **What this file is, and is not.** It was written when this repository was a
> library of WASI capability components, and its opening section still evaluates
> it as one: an auth + RBAC contract with a reference implementation. That is no
> longer what the repository is *for* — Holon is an agentic engineering loop built
> out of that library (see [`README.md`](README.md)) — but the per-showcase
> worklist below is still live and still accurate about what is done.
>
> For the current state, in order of usefulness:
> [`docs/CURRENT.md`](docs/CURRENT.md) (what runs, measured, and honestly
> missing), [`docs/SCENARIOS.md`](docs/SCENARIOS.md) (how a run succeeds and every
> way it fails), [`.comp/goals/`](.comp/goals/) (the worklist a person writes), and
> [`docs/adr/`](docs/adr/) (why any of it is shaped this way).
>
> Kept rather than deleted for the same reason `PLATFORM.md` is: its reasoning is
> the record of how the substrate got here.

## What this covers

One section per showcase: what is built, and what is left. The auth-era
evaluation and its Tier 1–5 roadmap lived here until every tier was done; they
are in git history rather than above, because a finished roadmap that still reads
as a plan is the kind of document people act on by mistake.

## TODO — next up

### Native wRPC-to-Golem capability provider — built (GOLEM.md)

- [x] **`providers/golem-workflow`** — the first *provider* in the repo (all else
      is components + hosts). A native wasmCloud v2 provider exporting
      `durable:workflow/orchestrator` over wRPC, bridging to durable **Golem**
      workers. **Live-verified end-to-end**: the bridge invokes a real deployed
      Golem 1.5 worker and its durable state advances (`just golem-e2e`,
      `GOLEM_E2E=1 cargo test`). Two findings baked into GOLEM.md: pin
      `wit-bindgen-wrpc 0.10` to match `wasmcloud-provider-sdk 0.17.1`'s
      `wrpc-transport 0.28`; and a direct `reqwest` gateway call beats the
      non-OSS, build-time-generated `golem-client`.
- [ ] Remaining: the wasmCloud-lattice *front* half (component → provider) needs
      the classic wasmCloud host — the installed `wash 2.3` is the new
      component-shell, so it's compiled-but-not-hosted-here. Plus the saga
      integration (a saga leg = a durable Golem worker). Original work brief:

<details>
<summary>Work brief (verbatim)</summary>

```
You are an expert systems-level Rust engineer specializing in the WebAssembly Component Model, wasmCloud (v2.x), and native wRPC (WebAssembly RPC) transports over NATS.

Your task is to write a complete, production-grade wasmCloud Capability Provider in Rust that acts as a native wRPC-to-Golem adapter.

Core Architecture
This provider runs as a native wasmCloud Capability Provider process. It implements the target WIT world using "wit-bindgen-wrpc" to automatically handle native, typed serialization and deserialization over NATS. Under the hood, it converts these typed Rust structs into Golem's universal "golem_wasm_rpc::Value" structure, triggers the durable Golem Worker via the Golem HTTP Client, and returns the strongly typed Rust result back to the caller over wRPC.

The control flow works as follows:

The wasmCloud Component makes a native wRPC call over the NATS Lattice.

The Golem wRPC Provider (this Rust application) intercepts the call.

The provider uses the "wit_bindgen_wrpc::generate!" macro to implement the generated asynchronous Rust Trait natively.

The provider directly maps typed Rust inputs to the Golem dynamic "Value" types.

The provider invokes Golem's REST API using the "golem_client::api::WorkerClient".

The provider maps Golem's returned values back to the strongly typed Rust return types.

Technical Specs and Dependencies
Generate a standard Rust binary crate project. Ensure your Cargo.toml targets these dependencies:

wit-bindgen-wrpc (For generating typed async Rust server bindings from WIT)

wasmcloud-provider-sdk (For handshake, linking, and managing the host-provider connection)

wrpc-transport-nats (For serving the generated wRPC bindings over the NATS bus)

golem-client (Golem's API Client SDK)

golem-wasm-rpc (with host feature enabled, for translating WIT types to Golem values)

tokio (multi-threaded async runtime)

tracing (structured logging)

The WIT Contract
Create a "wit/world.wit" file containing this exact contract:

Code snippet
package local:workflow;

interface orchestrator {
    record run-request {
        workflow-id: string,
        payload: string,
    }

    trigger-workflow: func(req: run-request) -> result<string, string>;
}

world golem-provider {
    export orchestrator;
}
Code Requirements
Please generate the following files:

Cargo.toml: Fully resolved dependencies using correct crate versions for a modern wasmCloud v2 ecosystem.

wit/world.wit: The interface definition provided above.

src/main.rs:

Invoke "wit_bindgen_wrpc::generate!({ world: "golem-provider" })" to build the traits.

Implement the async handler trait for the "orchestrator" interface. The signature must match the generated asynchronous signature, returning a standard "Result<Result<String, String>, ...>".

Instantiate Golem's "WorkerClient" during startup. Use environment variables (like GOLEM_URL and GOLEM_TEMPLATE_ID) passed during wasmCloud startup to configure the Golem endpoint.

Map the Rust types ("RunRequest" record) cleanly to "golem_wasm_rpc::Value" using helper mapping blocks (specifically matching a Record containing string fields).

In main(), initialize the wasmcloud-provider-sdk connection, obtain the NATS client, and pass the NATS transport directly to the generated serve function from the wRPC bindings to run the async server loop.

wadm.yaml: An application manifest showing how a frontend wasmCloud API component links to this provider to trigger durable workflows on Golem.

Ensure all Rust code is strictly typed, handles errors safely, and includes clean, descriptive error logging via the tracing crate.
```

</details>

### Helpdesk (HELPDESK.md)

- [ ] Rungs 2–7: multi-tenant + API keys + quotas, event-bus fan-out +
      notifications + signed webhooks, `mail-parse`, SLA timers + search,
      billing rollup, AI drafts. Rung 1 is done (`components/helpdesk-domain`,
      `examples/jco-helpdesk`, `just host-helpdesk` on the native host + NATS).

### Conduit / RealWorld (CONDUIT.md) — done

- [x] The full RealWorld ("Conduit") spec as one `conduit-domain` component +
      `auth-guard` + `record-store` + `slug`, run on the native Rust host.
      **Passes the official RealWorld Hurl conformance suite 100% (13/13 files,
      154 requests)** — `just conformance-conduit`; suite vendored + pinned under
      `examples/conduit/conformance`. Rust e2e (`just e2e-conduit`) + app-path
      bench (`bench/CONDUIT-BENCH.md`, round 13). This is the first showcase
      validated against an *external, objective* test suite rather than our own.
- [ ] Optional: password rotation + email rename in auth-guard (the two flagged
      conformance caveats), a `?search` extension (`search-index`), and a stock
      RealWorld frontend SPA served from `--static-dir`.

### Saga / durable orchestration (SAGA.md) — done

- [x] A durable **trip-booking saga** (`saga-domain`) — flight → hotel → car with
      **compensation** on failure — composed over `fsm-workflow` + `record-store`
      + `idempotency-guard` + `event-bus` + `sched:timer` + `id-generate`. The
      first showcase exercising **compensation + durable, resumable execution**:
      a flaky leg retries then gives up + compensates; `pump` advances one
      persisted step at a time; and the saga **survives a host kill and resumes**
      on NATS (`just durable-saga` → PASS). `just e2e-saga` (commit + all
      compensation/retry paths) + app-path bench (`bench/SAGA-BENCH.md`).
- [ ] Extract a generic `saga:orchestrator` contract (arbitrary step +
      compensation definitions), and land the Golem wRPC provider so a leg can be
      a real durable Golem worker (the roadmap item above).

### Realtime / pulse (REALTIME.md) — done

- [x] A realtime chat room (`pulse-domain`) — the first showcase in a new
      *class*: a **sustained connection with server push** rather than
      request/response. `GET /api/rooms/{room}/events` holds the HTTP response
      open and streams new messages as **Server-Sent-Events** `data:` frames
      (real push on wasip2 — the host streams the body while the guest loops;
      no WebSocket, no wasip3 async). Composed over `record-store` (log + cursor)
      + `event-bus` (fan-out spine) + `id-generate`, with a two-pane browser SPA
      (native `EventSource`) + presence. `just e2e-pulse` (a held-open reader
      gets a separately-posted message) + bench: one broadcast → **150/150
      concurrent SSE connections** (`bench/PULSE-BENCH.md`).
- [ ] Multi-host fan-out: replace per-stream polling with `event-bus` +
      `event-pusher` push, so connections can spread across hosts.
