# books — double-entry bookkeeping (the books always balance)

A chart of accounts and a journal, where **every entry must balance** — debits
equal credits — so the books can never drift. That invariant is the whole point,
and it lives in the **`ledger:doubleentry`** component: a lopsided entry is
rejected before it's ever stored. From the balanced journal the app derives the
three statements every set of books produces — a **trial balance**, a **profit &
loss**, and a **balance sheet** (assets = liabilities + equity) — and exports
them to PDF.

Same shape as the other showcases: one **`books-domain`** HTTP component that
exports `wasi:http` and imports only WIT contracts — the composed **auth-guard**
(`auth:identity`) for accounts, **`records:store`** for the chart + journal,
**`ledger:doubleentry`** for the invariant + trial-balance aggregation, and
**`pdf:codec`** for the statements. No bespoke auth, storage, accounting core, or
PDF writer. The frontend is a **React + shadcn/ui** SPA (Vite + Tailwind),
mobile-friendly, served by the host.

![The books app: the Journal tab has a double-entry editor — pick accounts, set debit/credit amounts, and a live badge flips from the running debits/credits to “balanced” only when they match; Post is disabled until then. The Reports tab shows a trial balance whose Debit and Credit totals are equal (a BALANCED badge), a profit & loss (income − expenses = net income), and a balance sheet with a BALANCES badge (assets = liabilities + equity + net income), plus a Statements PDF button. A live recording of the running React app.](../media/books.gif)

## The invariant (why `ledger:doubleentry`)

Accounting is trustworthy because of one rule: **for every entry, debits equal
credits**. `ledger:doubleentry` owns it — `validate(entry)` requires at least two
lines, positive amounts, and equal debits and credits, returning
`unbalanced(debits, credits)` otherwise. `books-domain` calls it on every post,
so an out-of-balance entry never reaches the store (`422`, with the mismatch).
`trial_balance(entries)` then aggregates a set of validated entries into
per-account totals whose grand totals are, necessarily, equal.

Amounts are **integer minor units** (cents) throughout — no floating-point
rounding to quietly break the invariant. The e2e posts a balanced entry, is
rejected on an unbalanced one (and on two-debits, and an unknown account), and
checks the trial balance, P&L, and balance sheet all reconcile.

## The data model

- **accounts** — `{code, name, type}` where `type` is
  asset / liability / equity / income / expense. The type sets the *normal* side
  (assets & expenses are debit-normal; the rest credit-normal), which is how a
  raw net (debits − credits) becomes a natural positive balance.
- **entries** — `{date, memo, lines: [{account, amount, side}]}`. Validated by
  `ledger:doubleentry` before storage; `owner`-scoped.

A fresh account is seeded a demo chart + a few entries so the reports aren't
empty.

## The reports (derived from the trial balance)

All three come from one `ledger::trial_balance` call plus the account types:

- **Trial balance** — every account's debit/credit totals; grand totals equal.
- **Profit & loss** — income accounts − expense accounts = **net income**.
- **Balance sheet** — assets, liabilities, equity, and the accounting identity
  **assets = liabilities + equity + net income** (current-period earnings).
- **`GET /api/reports/statement.pdf`** renders all three via `pdf:codec`.

## Run it

```bash
just host-books   # composes the component, builds the React UI, serves on :3045
# register a new account (seeded a demo chart + entries), post balanced journal
# entries, and read the trial balance / P&L / balance sheet (+ PDF).
just e2e-books    # balanced posts, unbalanced rejected, trial balances,
                  # balance sheet balances, statements PDF renders
```

The frontend lives in `examples/books/ui` (Vite + React + shadcn/ui); its journal
editor won't let you post until debits equal credits.

## Rungs left

- **CSV import** — bulk-post entries from a CSV via `csv:codec` (the parse side
  is already a component).
- **Periods + closing** — close income/expense into equity at period end so the
  balance sheet shows retained earnings instead of a live "net income" line.
- **Multi-currency** — carry a currency per account and revalue via `money:amount`.
