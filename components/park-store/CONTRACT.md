# park-store ↔ comp-park contract

`park-store` (this component) exports `holon:park/lot` (`wit/park/park.wit`) and
implements every call as ONE HTTP request to `comp-park` (native,
`reconciler/src/bin/park.rs`), which runs the `holon-park` engine over NATS
JetStream. Why the split: [ADR-0100](../../docs/adr/0100-a-parked-turn-is-a-record-not-a-held-thread.md)
and [ADR-0095](../../docs/adr/0095-what-is-allowed-to-be-native.md).
`park-gateway` serves the same routes, with the same JSON, to agents over
`comp-host`; this file is the wire for both. The contract is `holon:park@0.2.0`
([ADR-0106](../../docs/adr/0106-a-resumed-turn-knows-what-it-may-repeat.md));
0.1.0 callers break in two places, marked **0.2.0** below.

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
| `POST /v1/take-ready` | `{ticket}` | `resolution` — **0.2.0**, was a bare `call-result` |
| `POST /v1/interrupt` | `{ticket, reason, by: agent}` | `turn-status` (a bare string) — **0.2.0**, new |
| `POST /v1/cancel` | `{ticket, by: agent}` | `null` |
| `POST /v1/oplog` | `{session, after?: u64, limit}` | `[ticket-entry]` |
| `GET /health` | — | `{ok, backend, bucket, wake-stream}` |

### What `outbound-call`, `resolution` and `ticket-entry` look like

```json
{ "correlation": "job-42", "description": "nightly export", "deadline": 1700000000000,
  "poll": null, "replay": "safe" }
```

`replay` is `"safe"`, `"unsafe"` or `"abandon"` and is **required** by the WIT
(**0.2.0**); on this wire a caller that leaves it out is read as `"unsafe"`. What each means, and what
becomes of a call past its `deadline`, is the table in ADR-0106 and
`crates/holon-park/src/recover.rs`. A call with no `deadline` is never given up
on by anything.

`take-ready` answers exactly one of two single-key objects, and a client matches
on the key rather than on `ok`:

```json
{ "answered":    { "ok": true, "body": [42], "detail": null } }
{ "interrupted": { "reason": "…", "at": 1700000000000, "by": { "id": "comp-resume", "goal": null, "model": null } } }
```

`interrupted` means no answer will come **and the call may or may not have
happened**: check on the far side before repeating it.

`interrupt` answers the status the ticket is in afterwards: `"interrupted"`, or
`"ready"` if the answer landed first (and then it is the answer that `take-ready`
returns). Repeating it is `200` and the first `reason` stands; `already-closed`
once the ticket is `resumed` or `cancelled`; `invalid` with a blank `reason`, which
is what the turn is told.

A `ticket-entry` is `{ticket, session, status, parked-at, woken-at, resumed-at,
correlation, replay, deadline, redispatches, interrupted-at}`, `status` one of
`parked`, `ready`, `resumed`, `cancelled`, `expired`, `interrupted`. `correlation`
is how a restarted turn finds the call in its own journal: a ticket id is a hash.
`deadline` is the current one, which a re-dispatch pushes out.

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
| `already-closed` | string | 409 | `already-closed(detail)` — `take-ready`, `interrupt` or `cancel` on a ticket already `resumed` or `cancelled` |
| `invalid` | string | 400 | `invalid(detail)` — most often two sessions reusing one `correlation` |
| `bad-request` | string | 400 | `invalid("bad-request: …")` — the body was not the route's JSON |

Anything else — no `park-url`, nothing listening, the daemon dying mid-request,
a `401`, a body that is not this JSON, an `error` this side does not know — is
`storage-error` with what happened in its text. That is the one case the
contract says to retry, and it is the right one for "the store did not
answer".

## Rules

- **Idempotency.** `park` on an already-existing ticket (same `session` and
  `call.correlation`) never writes a second record, and a second `replay` or
  `deadline` is ignored; it reports `already-woken` if the ticket is already
  `ready` or `interrupted`, `already-parked` otherwise. `wake` against a ticket
  that is already `ready`, `interrupted`, `resumed` or `cancelled` is accepted and
  changes nothing — a redelivered webhook, or one that arrives after the ticket
  closed or was given up on, is not an error to the sender. `take-ready` is the
  one call that is NOT idempotent: a second call against an already-`resumed`
  ticket is `already-closed`.
- **A restarted turn reads `park` against its own `replay`** (ADR-0106):
  `already-parked` on a `safe` call means dispatch again, on an `unsafe` one
  means do not (wait, or `interrupt` if it will not), on an `abandon` one means
  `cancel`; `already-woken` means `take-ready`, whichever of the two it holds.
- **`correlation` must be unique across every session ever parked**, not just
  within one — it is what `wake` uses to find a ticket without knowing its
  session, so two different sessions racing to park the same `correlation` is
  refused (`invalid`), not silently misdelivered.
- **`expired`** is not a state anything writes; it is `pending`/`oplog`
  reading a still-`parked` ticket whose `call.deadline` has passed. A late
  `wake` against one still lands. What *does* act on an expired ticket is
  `comp-resume`'s recovery pass, below.
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

## `PARK_WAKE`, and `comp-resume`

`comp-park` announces on `PARK_WAKE` (a work-queue stream, subject
`<stream>.ticket`) once per real transition to something a turn can take: a
`wake` that moved a ticket to `ready`, and an `interrupt` that moved one to
`interrupted`. A message is `{"session", "ticket", "kind"}`, `kind` being
`"resolved"` (the default, and what a pre-0.2.0 `comp-park` sent) or
`{"redispatch": {"attempt": n}}`. It is a nudge, not the record: if a publish
fails the ticket has still moved, and a recovery pass announces it again.

`comp-resume` (`reconciler/src/bin/resume.rs`) is what reads it. It delivers each
resolved ticket to a sink over HTTP, at least once, and sweeps for calls that
never answered. Its sink contract — one `POST` of `holon_park::resume::Delivery`
with an `Idempotency-Key`, answered `2xx`, `410` or anything else — and its flags
are documented in that file's header and in ADR-0106.

