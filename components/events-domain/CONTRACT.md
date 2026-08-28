# `events:ticketing` — the contract

Free tickets for events with a hard capacity, a QR code per attendee, check-in by an
organizer, and swaps between attendees.

This file is the specification. **No part may edit it.** Four parts are written
independently against it, by four agents that never speak to each other, and the
only thing making their work fit together is that all four read this.

## Roles and authorisation

Every `/api/**` route needs a bearer token. Get a principal with

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

## Stored documents

Three collections. The shapes are fixed here because a part that invents its own
passes its own gate and fails the composition.

### `events`

```json
{ "title": "…", "starts_at": "2026-09-01T18:00:00Z", "capacity": 100,
  "organizer": "<principal.subject>", "state": "open" }
```

Indexed on `state` and `organizer`. `state` is `open` or `cancelled`.

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
| `POST /api/events` | organizer | 201 `{id, …}`; 400 on missing title/`starts_at`, or `capacity` < 1 |
| `GET /api/events` | any | 200 `{events:[…]}`; `?state=open` filters |
| `GET /api/events/{id}` | any | 200 the document plus `"id"`, `"claimed"` and `"remaining"`; 404 |
| `PATCH /api/events/{id}` | organizer, and only their own | 200; 403 if another organizer's; 404 |
| `DELETE /api/events/{id}` | organizer, own | 204. A **soft** delete: `state` becomes `cancelled`. Tickets already issued stay readable |

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

## Errors

Always `{"error":"snake_case_code"}`, with the codes named above. `500` only for a
store failure you cannot attribute.
