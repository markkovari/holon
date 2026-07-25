# transit — public-transport ticketing (buy a QR, validate with a camera)

Two roles. A **rider** buys a fare — a **single ride**, a **duration ticket**
(60 / 90 min), or a **monthly pass** — and gets a **QR code**. A **validator**
scans that QR with the device **camera** and the system decides **ACCEPT** or
**REJECT**, activating the ticket on first scan and enforcing its rules
thereafter: a single is consumed by one validation; a duration/pass gives
unlimited rides until its window lapses.

Same shape as the other showcases: one **`transit-domain`** HTTP component that
exports `wasi:http` and imports only WIT contracts — the composed **auth-guard**
(`auth:identity`) for accounts + RBAC, **`records:store`** for the fare catalog
and tickets, and **`qr:encode`** to render the scannable ticket. No bespoke auth,
storage, or QR encoder. The frontend is a **React + shadcn/ui** SPA (Vite +
Tailwind), mobile-first, served by the host — the validator screen drives the
device camera via the browser's native **`BarcodeDetector`** (no JS QR library).

![The transit app on a phone: a rider signs in, the Buy tab lists fares (single / 60-min / 90-min / monthly) and taps Buy; My tickets shows the ticket with a status badge and a Show button that reveals a big scannable QR; then a validator signs in to a camera scanner with a manual-entry fallback, pastes a ticket id and gets a big green ACCEPTED card, scans the same single ticket again and gets a red REJECTED "already used". A live recording of the running React app at a mobile viewport.](docs/media/transit.gif)

The rider UI is **Buy** + **My tickets** (each ticket shows its status and, while
scannable, its QR). The validator UI is a **camera scanner** (with a manual
paste field for browsers without a camera or `BarcodeDetector`) and a big
ACCEPT/REJECT result with the reason and any remaining time.

## The capability model

**Two global roles** (self-assigned at register in the demo; an operator would
grant them in prod):

| capability | rider | validator |
|---|:--:|:--:|
| buy a fare / show a ticket's QR | ✓ | ✓ (sees any) |
| validate a scanned code | ✗ | ✓ |

Every write checks the caller's token (`authorizer::introspect`); `POST
/api/validate` is validator-only. The QR payload is the ticket's **own
unguessable record id** — a fabricated id matches no record and is rejected, so
nothing needs to be signed.

## The correctness axis (single-use under concurrency)

The dangerous operation is *check-then-consume*: two validators scanning the same
**single** ticket at once must not both accept it. `transit` uses **optimistic
concurrency via the record store's revisions** (`records:store`'s built-in
compare-and-set): `validate` reads the ticket **and its revision**, decides, then
writes the activation back **guarded by that revision**. The losing writer gets a
`revision-conflict`, re-reads the now-activated ticket, and its second decision
sees "already used". No lock, no spin, always a definitive answer.

The e2e proves it: **8 validators** scan one single ticket at once; exactly
**one** gets `accept`, and the ticket ends `used` with `uses == 1`.

> This is the **optimistic** counterpart to [booked](BOOKED.md)'s **pessimistic**
> `lock:mutex` lease — two showcases, two standard ways to make a check-then-write
> safe under real parallelism (the host serves requests concurrently).

## The data model

- **fares** — the seeded catalog: `{key, name, kind, minutes, price}` where
  `kind` is `single` (one validation), `duration` (valid `minutes` from first
  scan), or `pass` (a 30-day monthly).
- **tickets** — `{rider, fare, fare_name, kind, minutes, price, purchased,
  activated, uses, history}`. `activated` (0 = not yet) starts the clock;
  `status` / `valid_until` / `remaining_min` are computed from it. `history` is
  the list of `{at, by}` validations.

## The QR + camera path

- `GET /api/tickets/{id}/qr.svg` renders the ticket's id as a QR **SVG** via
  `qr:encode` — the rider shows it on their screen.
- The validator's SPA opens the camera (`getUserMedia`, needs a secure context —
  localhost counts) and reads QR frames with the native **`BarcodeDetector`**
  API; the decoded id is `POST`ed to `/api/validate`. A manual paste field is the
  fallback where the camera or detector isn't available.

## Run it

```bash
just host-transit   # composes the component, builds the React UI, serves on :3042
# register as `rider` to buy tickets + show their QR;
# as `validator` to scan + validate.
just e2e-transit    # auth + fares + single-use (incl. an 8-way concurrency race)
                    # + duration window + a valid QR
```

The frontend lives in `examples/transit/ui` (Vite + React + shadcn/ui);
`just host-transit` builds it to `examples/transit/dist`, which the host serves.

## Rungs left

- **Anti-passback / rotating QR** — a static QR can be screenshotted and shared;
  rotate a per-ticket code on a `sched:timer` (or fold in `otp:totp`) so a scan
  only accepts the current window.
- **Signed offline validation** — sign the payload (`webhook-sign`) so a
  validator device can verify authenticity without a round-trip.
- **Zones + transfers** — fare rules by zone; a single that allows transfers
  within a grace window.
- **Operator console** — sales + validation reports (reuse `pdf:codec`), fare
  catalog editing.
