# `events:ticketing` — the contract

Free tickets for events with a hard capacity, a QR code per attendee, check-in by an
organizer, and swaps between attendees.

This file is the specification. **No part may edit it.** Four parts are written
independently against it, by four agents that never speak to each other, and the
only thing making their work fit together is that all four read this.

## Roles and authorisation

Every `/api/**` route except `register`, `login` and the notification stream
(which takes a signed ticket instead, below) needs a bearer token. Get a principal with

    authorizer::authorize(token, permission)     // -> principal, or auth-error

and never by parsing the token yourself. A missing or malformed bearer is **401**;
a valid token without the permission is **403**. `authorize` distinguishes them —
map its error, do not invent one.

| role | may |
|---|---|
| `attendee` | claim a ticket, see their own tickets, offer and accept swaps |
| `organizer` | everything an attendee may, plus create/update/delete events and check tickets in |
| `admin` | everything |

Permissions are `{ target, action }`:

| route | permission |
|---|---|
| events, write | `{ target: "event", action: "write" }` |
| events, read | `{ target: "event", action: "read" }` |
| tickets, claim/read | `{ target: "ticket", action: "write" }` / `"read"` |
| check-in | `{ target: "checkin", action: "write" }` |
| swaps | `{ target: "swap", action: "write" }` |

### Accounts — the router (`lib.rs`, scaffold)

| | | |
|---|---|---|
| `POST /api/register` | open | body `{"email","password"}`. 201 `{token, subject}` — already logged in. 400 `invalid` unless the email has an `@` and the password is 8+ characters; 409 `already_registered` |
| `POST /api/login` | open | 200 `{token, roles}`; 401 `bad_credentials` |
| `GET /health` | open | 200 `{"ok":true}` |
| `/test/**` | — | the fixture (`/test/seed`) and raw reads (`/test/{events,tickets,swaps}/{id}`); **404 unless `allow-test-routes` is `1` or `true`** in config |

Every new account is an `attendee`. Nobody can ask for a role: `organizer` is
granted — on registration AND on every login — to the addresses listed in the
`organizer-emails` config key (comma-separated, case-insensitive).

## Stored documents

Three collections. The shapes are fixed here because a part that invents its own
passes its own gate and fails the composition.

### `events`

```json
{ "title": "…", "starts_at": "2026-09-01T18:00:00Z", "capacity": 100,
  "organizer": "<principal.subject>", "state": "open",
  "description": "…",          // OPTIONAL — absent when not given, never ""
  "image_type": "image/png" }  // OPTIONAL — set when a poster is uploaded
```

Indexed on `state` and `organizer`. `state` is `open` or `cancelled`.

`description` is absent rather than empty when nobody wrote one: a caller reading
`""` cannot tell that from a description somebody cleared. `PATCH` with an explicit
`null` removes it.

The poster's BYTES are not in this document. They live in `blob:store` under the
container `event-images`, keyed by the event id, and the record keeps only the
content type — a JSON document is the wrong place for a JPEG, base64 is a third
larger than what it encodes, and every read of the event would pay for it.

### `tickets`

```json
{ "event_id": "<events id>", "holder": "<principal.subject>",
  "code": "<nanoid(21)>", "state": "issued",
  "issued_at": "…", "checked_in_at": null }
```

Indexed on `event_id`, `holder` and `code`. `state` is `issued`, `checked-in` or
`released`.

### `swaps`

```json
{ "ticket_id": "…", "from": "<subject>", "to": null,
  "state": "offered", "created_at": "…" }
```

Indexed on `ticket_id` and `state`. `state` is `offered`, `accepted` or `withdrawn`.

**`find_by` wants the JSON ENCODING of the value, not the value.** `record-store`
indexes the serialised form, so a string field `open` is indexed under `"open"` —
quotes included. `find_by("events", "state", "open")` matches nothing and returns
`Ok(vec![])`, which is indistinguishable from an empty collection. Use
`serde_json::to_string(&value)` and pass that.

## Capacity

The event's `capacity` is a hard ceiling on tickets in state `issued` or
`checked-in`. It is enforced with

    meter::reserve(subject, amount, limit, period_seconds)

where `subject` is `"event:<event_id>"`, `amount` is 1, `limit` is the event's
`capacity`, and `period_seconds` is `31_536_000` (a year — this is a fixed pool,
not a rate, and the period only has to outlive the event).

`reserve` returns `Err(quota-error::exceeded)` when the pool is empty; that is a
**409** with `{"error":"sold_out"}`.

**Counting the collection and comparing to `capacity` before creating the ticket is
wrong**, and it is wrong in a way that passes every test that issues tickets one at
a time. Two claims arriving together both read the same count and both create a
ticket. `reserve` is atomic; the gate issues the last two places concurrently and
requires exactly one 201 and one 409.

## The ticket lifecycle

Registered once with `fsm:workflow` under the machine name `ticket`:

- states `issued`, `checked-in`, `released`; initial `issued`; terminal `released`
- `check-in`: `issued` → `checked-in`
- `release`: `issued` → `released`

Each ticket is an instance whose id is the ticket's record id. An illegal move comes
back as `IllegalTransition(String)` carrying the **current** state, which is exactly
what the 409 body needs — do not look it up separately.

Both the fsm instance and the ticket document carry the state. Move both, or
`GET /api/tickets/{id}` disagrees with the machine.

## Routes

### Events — `events.rs`

| | | |
|---|---|---|
| `POST /api/events` | organizer | 201 `{id, …, reminder_at}` — `reminder_at` is when the reminder goes out (unix seconds), `null` if `starts_at` is unparseable (a start under 24 hours away is still scheduled, and is simply due at once); 400 on missing title/`starts_at`, or `capacity` < 1 |
| `GET /api/events` | any | 200 `{events:[…]}`; `?state=open` filters. **Every entry carries its `id`** alongside the document's own fields — a list nothing can be selected from is not a list |
| `GET /api/events/{id}` | any | 200 the document plus `"id"`, `"claimed"` and `"remaining"`; 404 |
| `PATCH /api/events/{id}` | organizer, and only their own | 200 the document plus `id`, and `reminder_at` when `starts_at` changed (the reminder is re-scheduled); 403 if another organizer's; 404 |
| `DELETE /api/events/{id}` | organizer, own | 204. A **soft** delete: `state` becomes `cancelled`. Tickets already issued stay readable. The reminder is cancelled and every holder is told (`event-cancelled`) |
| `POST /api/events/{id}/image` | organizer, own | the raw bytes, `Content-Type` naming the type. 201; 415 `type_not_allowed` / `too_large`; 400 `empty_body` |
| `GET /api/events/{id}/image` | any | the bytes under their stored content type; 404 `no_image` |
| `DELETE /api/events/{id}/image` | organizer, own | 204 |

What may be uploaded is **`upload:policy/gate::check(content-type, size)`**, which
reads `allowed-types` and `max-size` from config. Do not write a content-type match
arm: there are already three allowlists in this repository and a fourth that nothing
tests is worse than none.

`claimed` and `remaining` come from `meter::peek`, not from counting tickets.

### Tickets — `tickets.rs`

| | | |
|---|---|---|
| `POST /api/events/{id}/tickets` | attendee | 201 `{id, code, qr, …}`; 409 `sold_out`; 409 `already_holding` if this subject already holds a live ticket for this event; 404 if no event; 409 `event_cancelled` |
| `GET /api/tickets` | attendee | 200 `{tickets:[…]}` — only the caller's own |
| `GET /api/tickets/{id}` | holder, or organizer of the event | 200 with `qr`; 403 otherwise |
| `DELETE /api/tickets/{id}` | holder | 204; fires `release`, and the place returns to the pool |

`qr` is `encoder::svg(code, ecc::medium, 2)` — an SVG string. The QR carries the
`code` and nothing else.

Releasing a ticket must return its place: `meter::record_usage` cannot go negative,
so track releases as a separate reserve pool is **wrong**. Use `meter::reserve` for
the claim, and on release call `meter::reset` only if you can do it without freeing
everyone else's place — you cannot, so the correct move is to keep the reservation
and count `released` tickets out of `claimed` when reporting. A released place is
**not** re-issuable in this version, and `GET /api/events/{id}` must still report
`remaining` consistently with what `POST` will accept.

### Check-in — `checkin.rs`

| | | |
|---|---|---|
| `POST /api/checkin` | organizer | body `{"code":"…"}`. 200 `{ticket_id, event_id, holder, state:"checked-in"}` |

- unknown code → **404** `{"error":"no_such_ticket"}`
- already checked in → **409** `{"error":"already_checked_in","state":"checked-in"}`
- released ticket → **409** carrying the state the fsm reported
- the caller must be the organizer of *that* ticket's event, or admin → **403**

The scanner sends the decoded string. Decoding the image is the browser's job and
no part does it.

### Swaps — `swaps.rs`

| | | |
|---|---|---|
| `POST /api/swaps` | holder | body `{"ticket_id":"…"}`. 201 the offer; 409 if that ticket already has an `offered` swap; 403 if not the holder; 409 if the ticket is not `issued` |
| `GET /api/swaps` | any attendee | 200 `{swaps:[…]}` — the `offered` ones |
| `POST /api/swaps/{id}/accept` | any attendee who is not `from` | 200. The ticket's `holder` becomes the caller, the swap becomes `accepted` with `to` set. 409 if not `offered`; 403 if accepting your own |
| `DELETE /api/swaps/{id}` | `from` | 204, swap becomes `withdrawn` |

A swap moves a ticket between holders. **Capacity does not change** — no reserve, no
release. A part that re-reserves on accept will fail the composition gate, which
checks `remaining` is the same before and after a swap.

### Reminders and notifications — `remind.rs`, `notifications.rs`

An event's reminder is a `sched:timer` job keyed by the event, due 24 hours before
`starts_at` (`YYYY-MM-DDTHH:MM:SSZ`), scheduled when the event is created. Telling
someone anything goes through `notify:prefs`, which picks the channels (`in-app`,
`email`) from that person's own preferences; the app never picks one. Three things
are told: `event-reminder` to every live holder, `event-cancelled` to every holder
when the event is deleted, and `ticket-swapped` to a swap's `from` when it is
accepted. A `PATCH` that changes `starts_at` moves the reminder: the old job is cancelled
and, if the event is still `open`, a new one is scheduled 24 hours before the new start. The
`PATCH` answer then carries `reminder_at` (as `POST` does; `null` if nothing was scheduled). A
`PATCH` that leaves `starts_at` alone leaves the reminder alone and has no `reminder_at`.

| | | |
|---|---|---|
| `POST /api/reminders/run` | `event` write (organizer, admin) | fires whatever is due (a 60-second lease on up to 20 jobs), acks each after telling its holders; a job whose event is gone or not `open` is acked and dropped. 200 `{fired, reminders:[…]}`. Called by a scheduler in a deployment, a button in a demo |
| `GET /api/events/{id}/reminder` | `event` read | 200 `{scheduled:true, run_at, now, due_in_seconds}` or `{scheduled:false, now}`; 404 if no event |
| `GET /api/notifications` | `ticket` read | 200 `{notifications:[{seq, kind, title, body, payload, at, read}]}`, the caller's own, up to 50 after `?after=` |
| `GET /api/notifications/unread` | `ticket` read | 200 `{unread}` |
| `POST /api/notifications/read` | `ticket` read | body `{"seqs":[…]}` marks those, or `{"through":n}` (0 = all) marks everything up to n. 200 `{marked}` |
| `POST /api/notifications/stream-ticket` | `ticket` read | 200 `{ticket, ttl_seconds:60}` — a `webhook:sign`-signed, single-subject ticket, because `EventSource` cannot send a bearer |
| `GET /api/notifications/stream?ticket=…` | the ticket | SSE of the caller's notes, from `?after=`; 401 `bad_ticket` if it is bad or expired |
| `GET /api/prefs` | `ticket` read | 200 `{default_channels, email_address, overrides}` |
| `PUT /api/prefs` | `ticket` read | the same shape; the subject is always the caller's, never the body's. 200 `{ok:true}`; 400 if `notify:prefs` refuses it |

The stream ticket is signed with the `stream-ticket-secret` config key, and with a
fixed placeholder when that is unset.

## Imports

`records:store`, `id:generate`, `quota:meter`, `qr:encode`, `fsm:workflow`,
`auth:identity` (types, authorizer, accounts, rbac), `blob:store`,
`upload:policy`, `notify:prefs`, `notify:inbox`, `sched:timer`, `webhook:sign`,
`wasi:clocks` (wall and monotonic) and `wasi:config` — the last for
`allow-test-routes`, `organizer-emails`, `stream-ticket-secret` and the two keys
`upload:policy` reads. See `wit/events.wit`.

## Errors

Always `{"error":"snake_case_code"}`, with the codes named above. `500` only for a
store failure you cannot attribute.
