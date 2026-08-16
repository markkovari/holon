# books — double-entry bookkeeping (docs/apps/BOOKS.md)

A chart of accounts and a journal where **every entry must balance** (debits =
credits) — the invariant lives in the **`ledger:doubleentry`** component, so a
lopsided entry is rejected before it's stored. From the balanced journal the app
derives a trial balance, a profit & loss, and a balance sheet (assets =
liabilities + equity), exported to PDF via `pdf:codec`. See [docs/apps/BOOKS.md](../../docs/apps/BOOKS.md).

A composed HTTP app on the native Rust host, with a **React + shadcn/ui** SPA.

```
ui/                      # Vite + React + TS + Tailwind + shadcn/ui source
public/ -> (built)       # `npm run build` emits ../dist, which the host serves
tests/books.rs           # e2e: balanced/unbalanced entries + trial/P&L/balance-sheet + PDF
```

## Run

```bash
# from the repo root:
just host-books          # composes the component + builds the UI + serves on :3045
```

Open `http://127.0.0.1:3045`: **register** a new account — you get a demo chart +
entries. The **Journal** tab's double-entry editor won't let you post until
debits equal credits; the **Reports** tab has the trial balance, P&L, and balance
sheet (+ a Statements PDF).

```bash
just e2e-books           # the double-entry invariant + all three statements + PDF
# work on the UI live:
cd examples/books/ui && npm install && npm run dev
```
