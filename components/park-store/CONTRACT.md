# park-store ↔ comp-park contract

`park-store` (this component) exports `holon:park/lot` (`wit/park/park.wit`) and
implements every call as ONE HTTP request to `comp-park` (native,
`reconciler/src/bin/park.rs`), which runs the `holon-park` engine over NATS
JetStream. Why the split: [ADR-0100](../../docs/adr/0100-a-parked-turn-is-a-record-not-a-held-thread.md)
and [ADR-0095](../../docs/adr/0095-what-is-allowed-to-be-native.md).
`park-gateway` serves the same routes, with the same JSON, to agents over
`comp-host`; this file is the wire for both.

The JSON is defined once, in `crates/holon-park/src/wire.rs`, and every side —
daemon, store component, gateway — uses that definition.

## Config the component reads (`wasi:config`)

| key | meaning |
|---|---|
| `park-url` | where `comp-park` listens, e.g. `http://127.0.0.1:8015` (a base path is kept) |
| `park-token` | bearer token; must equal the daemon's `--token` (absent or empty = no header) |

## Routes

Every route is `POST /v1/<WIT function name>` with one JSON object, and answers
the function's `ok` value as the JSON body with `200` (or, for `wake`, a bare
JSON string). Every route except `GET /health` requires `Authorization: Bearer
<token>` when the daemon has one (`comp_reconciler::daemon_auth`); a wrong or
missing one is a bare `401`.

Records are the WIT's, field for field, kebab-case (`holon_park::model`): an
`option` is the value or `null`, and an absent `option` field is `null`.

| route | body | `200` body |
|---|---|---|
| `POST /v1/park` | `{session, call: outbound-call, by: agent}` | `park-result` |
| `POST /v1/wake` | `{correlation, answer: call-result}` | `ticket-id` (a bare string) |
| `POST /v1/pending` | `{session}` | `[ticket-entry]` |
| `POST /v1/take-ready` | `{ticket}` | `call-result` |
| `POST /v1/cancel` | `{ticket, by: agent}` | `null` |
| `POST /v1/oplog` | `{session, after?: u64, limit}` | `[ticket-entry]` |
| `GET /health` | — | `{ok, backend, bucket, wake-stream}` |

The gateway additionally answers `GET /` and `GET /v1` with the route list and
`GET /health` with `{ok: true}` (the component's own health, not the daemon's).

## Errors

Not `200`: the body is

```json
{ "error": "<case>", "detail": <payload>, "message": "<for people>" }
```

`error` is the `park-error` case, kebab-case; `detail` is a string for every
case here (unlike `vcs-error`, `park-error` has no structured payload);
`message` is never parsed. **Status codes are used, and are informational**: a
client decides by `error`, never by the status alone.

| `error` | `detail` | status | component receives |
|---|---|---|---|
| `storage-error` | string | 503 | `storage-error(detail)` — retry may help |
| `not-found` | string | 404 | `not-found(detail)` — a ticket that does not exist, or `take-ready` on one that is not `ready` yet |
| `already-closed` | string | 409 | `already-closed(detail)` — `take-ready` or `cancel` on a ticket already `resumed` or `cancelled` |
| `invalid` | string | 400 | `invalid(detail)` — most often two sessions reusing one `correlation` |
| `bad-request` | string | 400 | `invalid("bad-request: …")` — the body was not the route's JSON |

Anything else — no `park-url`, nothing listening, the daemon dying mid-request,
a `401`, a body that is not this JSON, an `error` this side does not know — is
`storage-error` with what happened in its text. That is the one case the
contract says to retry, and it is the right one for "the store did not
answer".

## Rules

- **Idempotency.** `park` on an already-existing ticket (same `session` and
  `call.correlation`) never writes a second record; it reports
  `already-woken` if the ticket is already `ready`, `already-parked`
  otherwise. `wake` against a ticket that is already `ready`, `resumed` or
  `cancelled` is accepted and changes nothing — a redelivered webhook, or one
  that arrives after the ticket closed, is not an error to the sender.
  `take-ready` is the one call that is NOT idempotent: a second call against
  an already-`resumed` ticket is `already-closed`.
- **`correlation` must be unique across every session ever parked**, not just
  within one — it is what `wake` uses to find a ticket without knowing its
  session, so two different sessions racing to park the same `correlation` is
  refused (`invalid`), not silently misdelivered.
- **`expired`** is not a state anything writes; it is `pending`/`oplog`
  reading a still-`parked` ticket whose `call.deadline` has passed. A late
  `wake` against one still lands.
- **Bodies** up to 4 MiB (`park-gateway`'s `MAX_BODY_BYTES`) — every body here
  is small JSON, never a file tree.
- **Timeouts.** The component allows five minutes per call.

## Flags

| flag | default | |
|---|---|---|
| `--addr` | `127.0.0.1:8015` | |
| `--token` / `--token-file` | none | bearer secret (`daemon_auth`) |
| `--nats-url` (`PARK_NATS_URL`) | `nats://127.0.0.1:4222` | JetStream: ticket records and indexes (KV), `PARK_WAKE` (a work-queue stream) |
| `--bucket` | `holon-park` | the KV bucket |
| `--wake-stream` | `PARK_WAKE` | the wake-notification stream's name |
| `--memory` | off | everything in memory, nothing durable, no `PARK_WAKE` — for trying it and for tests |
