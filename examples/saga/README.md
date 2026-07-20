# saga — a durable trip-booking saga on the native Rust host

The textbook distributed **saga** as ONE composed wasm HTTP component
(`saga-domain` + fsm-workflow + record-store + idempotency-guard + event-bus +
id-generate + scheduler-timer), served by the native Rust host. Book flight →
hotel → car; if any leg fails, **compensate** the booked legs in reverse. See
[`../../SAGA.md`](../../SAGA.md).

The axis no other showcase covers: **compensation + durable, resumable
execution**. State lives entirely in `wasi:keyvalue` (records + fsm), so a saga
survives the host process dying mid-flight.

![commit → compensation → durability restart](../../docs/media/saga.gif)

## Verify

```bash
just e2e-saga        # commit + compensation + retry (recover & give-up), memory KV
just durable-saga    # NATS: start a saga, KILL the host mid-flight, restart → it resumes
just host-saga       # serve it yourself on :3012
```

```bash
ID=$(curl -s localhost:3012/trips -d '{"traveler":"Ada"}' | jq -r .id)
curl -s localhost:3012/trips/$ID/run | jq          # → committed, 3 booked legs
curl -s localhost:3012/trips -d '{"traveler":"Grace","failLeg":"car"}'   # then /run → compensated
```

## What runs where

| Layer | Language |
|---|---|
| `saga-domain` (the orchestrator: step machine + compensation) | Rust → wasm |
| fsm-workflow, record-store, idempotency-guard, event-bus, scheduler-timer | Rust → wasm |
| host serving the composed `.wasm` | Rust (`host/`, wasmtime) |
| e2e (`ureq`) + durability proof (`durability.sh`) | Rust / bash |
