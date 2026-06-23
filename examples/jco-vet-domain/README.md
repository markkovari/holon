# jco-vet-domain — the vet-clinic domain as ONE wasm HTTP component

The vet-clinic backend, recreated **language- and host-agnostic**: the
pet/appointment/note domain that used to be TypeScript glue (`jco-vet-clinic`)
is here the **`vet:domain`** Rust component, composed with every capability it
needs into a single self-contained `.wasm` that exports
`wasi:http/incoming-handler`.

```
vet_domain.composed.wasm
  = vet-domain (Rust HTTP handler)         exports wasi:http/incoming-handler
  + auth-guard.composed                    auth:identity (accounts/session/authorizer/rbac)
  + record-store                           records:store (pets/appointments/notes)
  + validate                               validate:schema (request bodies)
  + search-index                           search:index (pet search)
```

Built with `just compose-vet`. The only remaining imports are generic WASI
(`keyvalue`, `clocks`, `random`, `config`, `http`) — bound by the host.

## The point: nothing is language- or host-locked

- **Language-agnostic** — the contract is `wit/vet.wit`. The impl is Rust today;
  a different team could rewrite `vet_domain.wasm` in Go/C against the exact same
  world and drop it into the same composition.
- **Host-agnostic** — the component exports `incoming-handler`, the same shape a
  wasmCloud `http-server` provider drives. This example serves it in-process via
  jco's WASI `HTTPServer` over real Node HTTP; a wasmCloud host runs the
  **identical bytes**. (`examples/vet-clinic-wasmcloud` is the k8s deploy path.)
- **Backend-agnostic** — `wasi:keyvalue` here is an in-memory shim; point it at
  NATS/redis/sqlite (or the wasmCloud keyvalue-nats provider) and the component
  is unchanged.

## Run

```bash
# from comp/: build + compose the app wasm
just compose-vet            # -> components/target/vet_domain.composed.wasm
cp components/target/vet_domain.composed.wasm examples/jco-vet-domain/

cd examples/jco-vet-domain
npm install
npm test                    # serves the wasm via WASI HTTPServer, drives it over HTTP
npm start                   # serve on :3005 for manual curl
```

## What the test proves (all over real HTTP, no Node domain code)

seed RBAC → register owner/doctor → login → owner adds a pet (validated by
validate:schema, indexed by search:index, stored in records:store) → search
finds it → book an appointment → owner is **403** on a visit note (lacks
`notes:write`) → doctor (has it) writes the note **201**. Missing token → **401**.

## Routes

| method | path | guard |
|---|---|---|
| POST | `/register` `{email,password,role?}` | — (assigns role) |
| POST | `/login` | — |
| GET | `/me` | bearer |
| GET | `/pets[?q=]` | `pets:read` (owners see own) |
| POST | `/pets` | `pets:write` |
| GET | `/appointments` | `appointments:read` |
| POST | `/appointments` | `appointments:write` |
| POST | `/appointments/{id}/notes` | `notes:write` |
| POST | `/admin/role-permissions`, `/admin/assign-role` | — (seed; guard in prod) |

## Scope

Core slice — pets/appointments/notes + auth/RBAC/validation/search. The
`jco-vet-clinic` JS example additionally has photos, fsm lifecycle, money
invoices, markdown, csv, otp, i18n, pagination, pii, ai-summaries, timers, lock,
event-bus; porting those into `vet-domain` is a later parity pass (each is one
more WIT import + plug).
