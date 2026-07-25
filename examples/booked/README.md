# booked — a Calendly-lite booking service (BOOKED.md)

An owner publishes bookable **resources** with weekly **availability**; anyone
books a free slot — and **can't double-book**: a booking takes a `lock:mutex`
lease over its check-then-write, so racing bookers get a `409`, never a clash.
Recurring bookings expand via `rrule:recur`; bookings export to `.ics` via
`ical:codec`; the confirmation is an `email:template`. See [BOOKED.md](../../BOOKED.md).

A composed HTTP app on the native Rust host, with a **React + shadcn/ui** SPA.

```
ui/                      # Vite + React + TS + Tailwind + shadcn/ui source
public/ -> (built)       # `npm run build` emits ../dist, which the host serves
tests/booked.rs          # e2e: auth + availability + no-double-book (concurrency) + recurrence + .ics
```

## Run

```bash
# from the repo root:
just host-booked         # composes the component + builds the UI + serves on :3041
```

Open `http://127.0.0.1:3041`: **register** as `owner` to create resources and set
weekly availability, or as `member` to book. Pick a resource + day, tap a free
slot to book it (toggle **weekly** to repeat for N weeks); the **My bookings** tab
has `.ics` download + cancel.

```bash
just e2e-booked          # auth + availability + no-double-book + concurrency race + recurrence + .ics
# work on the UI live:
cd examples/booked/ui && npm install && npm run dev
```
