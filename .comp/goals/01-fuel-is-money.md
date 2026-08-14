# Fuel is money — 🟡 needs a gate

**Traces to:** `docs/CURRENT.md` — *"Tokens are not money."* A run reports
`spent-tokens`; a project has a budget field nothing spends against; and the two
are in different units nothing converts between.

## What is wanted

A pure function that turns a token count and a model name into a cost, so a
budget can be stated and enforced in the one unit a person actually cares about.

```
cost_cents(prompt_tokens, completion_tokens, model) -> u64
```

- A per-model price table (input and output priced separately, as every provider
  prices them). Unknown model → the most expensive known tier, never zero: a
  budget that treats an unknown model as free is not a budget.
- Rounds up. Underspending a cap is fine; overspending it because of a floor is
  the failure this exists to prevent.

Then `generation::search`'s `max-tokens` bound gains a sibling `max-cents`, and
`spent-tokens` a `spent-cents`, computed from each attempt's model and usage.

## Surface

- **writable:** `reconciler/src/generation.rs`
- **gate:** `cargo test -p comp-reconciler --lib generation::` (write the price
  and rounding assertions first — that is the 🟡)

## Why this shape

Cost is the only budget unit that survives a tier router: haiku and opus differ
by an order of magnitude, so a search that switches tiers mid-run cannot be
bounded by token count. It is deliberately a pure function — the prices are the
only thing that will ever be wrong, and a pure function is where a wrong price is
a one-line fix with a test beside it.
