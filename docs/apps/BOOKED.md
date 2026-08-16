# booked — a Calendly-lite booking service (no double-books)

An **owner** publishes bookable **resources** (a room, a person, a piece of kit),
each with weekly **availability** windows; anyone books a free slot. The headline
is **correctness under concurrency**: two people racing for the same slot must not
both win. A booking takes a `lock:mutex` lease on `book:{resource}:{day}` and runs
its overlap-check-then-write *inside* that critical section, so the second racer
sees the conflict and gets a `409` — never a double-book.

Same shape as the other showcases: one **`booked-domain`** HTTP component that
exports `wasi:http` and imports only WIT contracts — the composed **auth-guard**
(`auth:identity`) for accounts + RBAC, **`records:store`** for the data,
**`lock:mutex`** for the no-double-book guarantee, **`email:template`** for the
confirmation, and two new pure-compute codecs: **`ical:codec`** (`.ics` export)
and **`rrule:recur`** (recurring bookings). No bespoke auth, storage, locking,
calendar format, or recurrence math. The frontend is a **React + shadcn/ui** SPA
(Vite + Tailwind), mobile-friendly, served by the host.

![The booked app on a phone: an owner signs in, the Manage tab creates a resource and toggles weekly availability (Mon–Fri 09:00–17:00); the Book tab picks a day and shows the free 30-minute slots as buttons — tap one to book it and a confirmation card appears with the rendered email and an "Add to calendar" .ics; a second tap on the same slot is refused; a weekly-repeat toggle books the slot for N weeks at once; the My bookings tab lists them with .ics download and cancel. A live recording of the running React app at a mobile viewport.](../media/booked.gif)

The SPA has three surfaces: **Book** (pick a resource + day → free slots → tap to
book, optionally repeating weekly), **My bookings** (upcoming list with `.ics`
download + cancel), and an owner-only **Manage** tab (create resources, edit each
weekday's availability, grab the resource's `.ics` feed URL).

## The capability model

**Two global roles** (self-assigned at register in the demo; an admin would grant
them in prod):

| capability | member | owner |
|---|:--:|:--:|
| book a free slot / cancel **their own** booking | ✓ | ✓ |
| book a slot that's taken or outside availability | ✗ | ✗ |
| create resources / set weekly availability | ✗ | ✓ |
| see **every** booking + a resource's whole feed | ✗ | ✓ |

Every write checks the caller's token (`authorizer::introspect`). A booking must
fall inside an availability window for that weekday (a resource with *no* windows
is treated as always-open, so it's usable out of the box).

## The correctness axis (why `lock:mutex`)

The dangerous operation is check-then-write: *is this slot free? → yes → write
the booking*. Two concurrent bookers can both read "free" and both write — a
double-book. `booked` closes that window: `book_one` acquires a lease on
`book:{resource}:{day}`, re-reads the day's bookings **inside** the lease,
checks overlap, writes, then releases. The lease's TTL is a dead-man's switch, so
a crashed booker never wedges the slot.

The e2e proves it: **8 threads** POST the same slot at once; exactly **one** gets
`201`, the rest `409`, and the store ends with **exactly one** booking for that
slot — regardless of how the host schedules the requests.

## The data model

- **resources** — `{key, name, owner, slot, tz}`; `slot` is the default booking
  length in minutes.
- **availability** — `{resource, weekday (Mon=0…Sun=6), start, end}` in
  minutes-from-midnight; the weekly windows a booking must fit inside.
- **bookings** — `{resource, resource_name, user, email, day, start, end, note}`.
  `day` is `YYYY-MM-DD` and times are minutes, so an overlap check is integer
  compares and the client owns the calendar — no server-side date math beyond
  weekday lookup.

## Recurring bookings + `.ics` export

- **Repeat** — a booking may carry `repeat: {freq, interval?, weekdays?, count?,
  until?}`. `rrule:recur` expands it (from the booking day, over a bounded window)
  into instance days; each instance is conflict-checked independently and the
  response reports which booked and which clashed. "Book every Tuesday for 8
  weeks" in one call.
- **`.ics`** — `GET /api/bookings/{id}.ics` renders one booking, and
  `GET /api/resources/{id}/calendar.ics` renders a subscribable feed, both via
  `ical:codec` (RFC 5545: CRLF, 75-octet folding, escaping, UTC timestamps, a
  VALARM reminder). Both new codecs are pure compute and reusable by any showcase.

> ponytail: times are emitted as UTC in the `.ics`; a real multi-region deploy
> would carry each resource's `tz` (already stored) into `DTSTART`/`DTEND`.

## Run it

```bash
just host-booked    # composes the component, builds the React UI, serves on :3041
# register as `owner` to create a resource + weekly availability;
# as a `member` to book free slots.
just e2e-booked     # auth + availability + no-double-book (incl. a concurrency
                    # race) + recurrence expansion + a valid .ics
```

The frontend lives in `examples/booked/ui` (Vite + React + shadcn/ui);
`just host-booked` builds it to `examples/booked/dist`, which the host serves.

## Rungs left

- **Timezones** — carry each resource's `tz` into slot math + `.ics` (stored, not
  yet applied).
- **Booking-changes feed** — bump a `SEQUENCE` on reschedule so subscribers see
  updates; cancellations as `STATUS:CANCELLED`.
- **Real email** — hand the rendered `email:template` message to `notify:dispatch`
  instead of returning it in the response.
- **Buffers + lead time** — min-notice and gap-between-bookings knobs per resource.
