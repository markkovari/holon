# park-store ↔ comp-park contract

`park-store` (this component) exports `holon:park/lot` (`wit/park/park.wit`) and
implements every call as ONE HTTP request to `comp-park` (native,
`reconciler/src/bin/park.rs`), which runs the `holon-park` engine over NATS
JetStream. Why the split: [ADR-0100](../../docs/adr/0100-a-parked-turn-is-a-record-not-a-held-thread.md)
and [ADR-0095](../../docs/adr/0095-what-is-allowed-to-be-native.md).
`park-gateway` serves the same routes, with the same JSON, to agents over
`comp-host`; this file is the wire for both. The contract is `holon:park@0.4.0`
([ADR-0106](../../docs/adr/0106-a-resumed-turn-knows-what-it-may-repeat.md) added
`replay` and `interrupted`, [ADR-0109](../../docs/adr/0109-a-call-may-fan-out.md) added
groups, [ADR-0110](../../docs/adr/0110-several-calls-register-together-or-not-at-all.md) added
`commit`). Callers of 0.1.0 break in two places, marked **0.2.0** below, and callers of 0.2.0
in one more, marked **0.3.0**: `take-ready` can now return `joined`. 0.4.0 adds a function and
types and breaks nobody.

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
<token>` when the daemon has one (`comp_daemon_kit`); a wrong or
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
| `POST /v1/commit` | `{session, effects: [effect], by: agent}` | `commit-result` — **0.4.0**, new |
| `POST /v1/cancel` | `{ticket, by: agent}` | `null` |
| `POST /v1/oplog` | `{session, after?: u64, limit}` | `[ticket-entry]` |
| `GET /health` | — | `{ok, backend, bucket, wake-stream}` |

### What `outbound-call`, `resolution` and `ticket-entry` look like

```json
{ "correlation": "job-42", "description": "nightly export", "deadline": 1700000000000,
  "poll": null, "replay": "safe" }
```

`replay` is `"safe"`, `"unsafe"` or `"abandon"` and is **required** by the WIT
(**0.2.0**); on this wire a caller that leaves it out is read as `"unsafe"`. What each
means, and what becomes of a call past its `deadline`, is the table in ADR-0106 and
`crates/holon-park/src/recover.rs`. A call with no `deadline` is never given up
on by anything. `parent` and `group` (**0.3.0**) are optional and make a call a child or a
group: see *Groups* below.

`take-ready` answers exactly one single-key object, and a client matches
on the key rather than on `ok`:

```json
{ "answered":    { "ok": true, "body": [42], "detail": null } }
{ "interrupted": { "reason": "…", "at": 1700000000000, "by": { "id": "comp-resume", "goal": null, "model": null } } }
```

`interrupted` means no answer will come **and the call may or may not have
happened**: check on the far side before repeating it. A third shape, `joined`
(**0.3.0**), is what a *group* resolves to:

```json
{ "joined": { "outcome": "failed-fast", "missing": 0, "children": [
    { "ticket": "…", "correlation": "c1", "ended": { "answered": { "ok": true, "body": [1], "detail": null } } },
    { "ticket": "…", "correlation": "c2", "ended": { "interrupted": { "reason": "…", "at": 1700000000000, "by": { "id": "turn" } } } },
    { "ticket": "…", "correlation": "c3", "ended": "cancelled" } ] } }
```

`interrupt` answers the status the ticket is in afterwards: `"interrupted"`, or
`"ready"` if the answer landed first (and then it is the answer that `take-ready`
returns). Repeating it is `200` and the first `reason` stands; `already-closed`
once the ticket is `resumed` or `cancelled`; `invalid` with a blank `reason`, which
is what the turn is told.

A `ticket-entry` is `{ticket, session, status, parked-at, woken-at, resumed-at,
correlation, replay, deadline, redispatches, interrupted-at, parent, group}`, `status` one of
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

## Groups (ADR-0109)

A call parked with `"group": {"policy": "all-settled" | "fail-fast", "children": N}` is a
**group**: it is resolved by its children, not by `wake`. A call parked with
`"parent": "<the group's correlation>"` is a **child**. Parent and child are in the same
session, and the group is parked first.

| | |
|---|---|
| `all-settled` | resolves when all `N` children have been parked and have settled, however each ended |
| `fail-fast` | resolves as soon as one child fails (an `ok: false` answer, an interruption, a cancellation); the children still parked are cancelled **first** |
| the group's own `deadline` | resolves it `timed-out` if it is still waiting: the children still parked are cancelled first, and `missing` counts the ones never parked |
| `cancel` a group | cancels its children first, then the group: bottom-up |
| `cancel` a child | counts as a failure to its group, which may resolve it |

`take-ready` on a group returns `joined`: every child in the order it was parked, how each
ended, and why the group stopped waiting (`all-answered`, `some-failed`, `failed-fast`,
`timed-out`). The children are consumed with it.

What is refused, all `invalid` (`400`): a child whose group does not exist, is not a group, is
another session's, or has already resolved; a child past the `N` declared; a group that is also
a child (groups do not nest); `children` of 0 or over 1024; `wake` or `interrupt` on a group;
`take-ready` or `resolution` on a child ("take the group"); re-parking a child under a different
parent. `replay` on a group is ignored: each child has its own, and is recovered on its own.

## Commits (ADR-0110)

`commit` applies several effects together, or not at all. An effect is one of four, each what
the call of that name does:

```json
{ "park":      { "correlation": "c1", "description": "…", "deadline": null, "poll": null, "replay": "unsafe", "parent": "g" } }
{ "wake":      { "correlation": "c1", "answer": { "ok": true, "body": [], "detail": null } } }
{ "interrupt": { "correlation": "c1", "reason": "…" } }
{ "cancel":    "c1" }
```

and the answer is `{"batch": "<id>", "parked": [park-result, …]}`, one entry per `park`, in the
order listed.

- **Admission is all or nothing.** Every effect is checked before anything is written; a list with
  one bad effect (`invalid`, `400`) registers none of them. Checked: at least one effect and at
  most 256; no correlation parked twice; every `park` valid as `park` is, a child's group parked
  earlier in the same list or already there and open, and no group given more children than it
  was declared with; no correlation another session owns; every `wake`, `interrupt` and `cancel`
  naming a call the list parks or one that exists, and none `wake`-ing or `interrupt`-ing a group.
- **Then the intent is written**, as one record holding the whole list, and from there the effects
  are all applied or all undone: by this call, and if it dies by `comp-resume`'s sweep once the
  intent has sat for `--batch-lease-secs`.
- **Parks first, in order**; then `wake`, `interrupt` and `cancel`, in order. A `park` is the one
  effect that can still be refused after the intent exists (another session may have taken a
  correlation in the meantime), and the one that can be undone, so if one is refused every ticket
  *this commit* made is cancelled (last first, so a group's children go before the group) and the
  commit is **aborted**: `invalid` with the reason. A ticket that was already there is never
  cancelled by it. The other three cannot be undone, so none is applied until every `park` has
  succeeded.
- **Idempotent.** The identical list is the same commit (`batch` is a hash of the session and the
  effects in order). Again, it is found done and each park reports `already-parked`, or it is
  finished from where it stopped. A `wake`, `interrupt` or `cancel` that finds its call already
  closed is the effect already done.
- **Not isolated.** While a commit is pending a reader may see some of its tickets and not others.
  It is atomic in what lasts, not in what is seen on the way.
- What the effects resolve is announced: a `wake`, `interrupt` or `cancel` of a child that resolves
  its group puts the group on `PARK_WAKE`, as it would outside a commit.

## `PARK_WAKE`, and `comp-resume`

`comp-park` announces on `PARK_WAKE` (a work-queue stream, subject
`<stream>.ticket`) once per real transition to something a turn can take: a
`wake` that moved a ticket to `ready`, an `interrupt` that moved one to
`interrupted`, and a call (a `wake`, `interrupt` or `cancel` of a child) that
resolved a group. A child is never announced on its own: its group is. A message is `{"session", "ticket", "kind"}`, `kind` being
`"resolved"` (the default, and what a pre-0.2.0 `comp-park` sent) or
`{"redispatch": {"attempt": n}}`. It is a nudge, not the record: if a publish
fails the ticket has still moved, and a recovery pass announces it again.

`comp-resume` (`reconciler/src/bin/resume.rs`) is what reads it. It delivers each
resolved ticket to a sink over HTTP, at least once, and sweeps for calls that
never answered. Its sink contract — one `POST` of `holon_park::resume::Delivery`
with an `Idempotency-Key`, answered `2xx`, `410` or anything else — and its flags
are documented in that file's header and in ADR-0106.

