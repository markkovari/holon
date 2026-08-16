# transit — public-transport ticketing (docs/apps/TRANSIT.md)

A **rider** buys a fare (single / 60-min / 90-min / monthly) and gets a **QR**
(rendered by `qr:encode`); a **validator** scans it with the device **camera**
(the browser's native `BarcodeDetector`) and the system decides ACCEPT / REJECT.
A **single** ticket is consumed by one scan — enforced under concurrency by
`records:store`'s revision CAS (exactly one of N racing scans wins). See
[docs/apps/TRANSIT.md](../../docs/apps/TRANSIT.md).

A composed HTTP app on the native Rust host, with a **React + shadcn/ui** SPA.

```
ui/                      # Vite + React + TS + Tailwind + shadcn/ui source
public/ -> (built)       # `npm run build` emits ../dist, which the host serves
tests/transit.rs         # e2e: auth + fares + single-use (concurrency race) + duration + QR
```

## Run

```bash
# from the repo root:
just host-transit        # composes the component + builds the UI + serves on :3042
```

Open `http://127.0.0.1:3042`: **register** as `rider` to buy fares and show their
QR, or as `validator` to scan + validate. The validator screen uses the device
camera (localhost is a secure context, so `getUserMedia` works) with a manual
paste fallback.

```bash
just e2e-transit         # auth + fares + single-use + 8-way concurrency race + duration + QR
# work on the UI live:
cd examples/transit/ui && npm install && npm run dev
```
