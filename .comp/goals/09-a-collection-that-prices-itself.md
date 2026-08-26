# A collection that prices itself — ✅ all three landed, written by the loop

**Traces to:** the loop being [paused because the gate is the limit](../../README.md#the-agentic-loop--paused-and-kept),
and to ADR-0089 (*reuse before build*), ADR-0094 (*a capability describes itself
in a caller's words*).

## What is being built

A Pokémon TCG collection that you photograph rather than type: shoot a card, a
vision model guesses the name, set, number, variant and condition, and you correct
what it got wrong. From there it is a portfolio — what each card is worth, what it
cost, what selling has already made — and a marketplace, where a swap or a sale
hands custody over by QR.

One segment on purpose. "A TCG app" is four card models and four price sources;
Pokémon alone has enough shape (set codes, `058/165` numbering, reverse holos, 1st
Edition, PSA slabs, Japanese prints) to be a real problem, and none of the
generality is wasted if a second game arrives later.

### Why this app, and not a smaller one

The loop is paused because **a gate that already passes on the base tree accepts
anything**, and the first decomposed run scored a perfect 1000 on two candidates
that had deleted their own component exports. An app escapes that trap almost for
free, and this one especially:

- Its hard parts are **pure compute over money and time** — FIFO cost basis, a
  price series across gaps, a model answer parsed into typed fields. A test either
  passes or it does not, and none of it needs a network, a database or a key.
- Every one of those tests **fails on an empty function**, so no gate here is
  vacuous. That is the property goal 07 is about, obtained by construction rather
  than by criticism.
- Getting them *plausibly* wrong is easy and expensive, which is what makes them
  worth gating. Average cost instead of FIFO, a straight line drawn across a
  weekend the market did not trade in, a condition silently defaulted to Near
  Mint — each produces a number that looks right on a chart and is not.

## What already exists, and is not to be rebuilt

ADR-0089's rule applies to this goal harder than most. `just capsearch` before
writing anything; the pool already holds:

| for | use |
|---|---|
| the QR a swap hands over | `qr:encode` |
| signing that QR so it cannot be forged or replayed | `webhook:sign` (HMAC over a timestamped payload) + `idempotency:guard` (claimed once, ever) |
| charts | `svg:chart` — line, bar, sparkline, server-rendered |
| money | `money:amount` — integer minor units, tagged currency, never a float |
| accounts, sessions, RBAC | `auth:identity` |
| the photo on the way in | `upload:policy` → `blob:store` → `media:image` |
| storing and querying cards | `records:store` + `search:index` |
| double-entry, if the marketplace needs a book | `ledger:doubleentry` |
| polling a price source on a schedule | `sched:timer` + `cron:expr`, driven by `comp-relay` (ADR-0096) |
| the vision call itself | the shape `components/photo-critic` already proves — egress, key from the vault, an image block |

**There is no `custody:transfer` contract in this plan, deliberately.** A signed
one-shot token is `webhook:sign` plus `idempotency:guard`, and a third contract
wrapping two existing ones would be a new interface that adds no capability.

## The three goals

Each is one crate, one writable file, and a held-out test file that is the
specification. **All three are done**, and the loop wrote every line of every one:

| goal | tests | model calls | attempts |
|---|--:|--:|--:|
| `card-identify` | 19 | 1 | 1 |
| `portfolio-value` | 16 | 1 | 1 |
| `price-history` | 13 | 2 | 2 |

48 held-out tests, 4 model calls. The first run of `card-identify` took **42** and
kept nothing — what changed in between was the harness, not the model, and it is
written up in the commit that fixed it. The goal specs below are what ran.

```toml
# .comp/goal.toml — one of the three, per run
text = "…the goal text below…"
component = "portfolio-value"     # derives base_paths, workspace_manifest, keep_members
writable = ["components/portfolio-value/src/lib.rs"]

[[check]]
name = "the specification"
run = "cd components && cargo test -p portfolio-value"
```

Swap `portfolio-value` for `price-history` or `card-identify`. The test file is
**not** in `writable`, which is the whole point: a candidate that cannot pass the
spec cannot edit the spec.

### 1. `portfolio-value` — 16 tests, in four weighted checks

What a collection is worth, what it cost, and what selling has already made. FIFO
lots, realised against unrealised, a value series for the chart.

The cases that carry the goal: a sale consumes the **oldest** lot (average cost
gets a plausible wrong answer); an unpriced holding is carried at **cost** and
counted, because zero makes the chart dip and dropping it makes the chart climb;
two currencies are refused rather than converted; events arriving out of order —
the normal case when somebody backfills a 2019 purchase — give the same answer.

### 2. `price-history` — 13 tests, in three weighted checks

Sparse, duplicated, disagreeing quotes into the series a chart is drawn from.

The cases that carry the goal: a gap **carries the last price forward and says it
did**, never interpolates — a straight line across a quiet week invents most of a
five-year chart; samples before the first quote are **absent, not zero**; a
"lowest listing" never leaks into a market series; two sources disagreeing about
one day resolve the same way whatever order they were fetched in.

### 3. `card-identify` — 19 tests

A vision model's answer into typed fields, plus the list of fields a person should
check. The prompt lives in the same crate as the parser so the two cannot drift.

The cases that carry the goal: a photo that is **not a card** is an error and
never a blank row; an absent field is **never defaulted** — a collection where 300
cards silently say "Near Mint" is worth an unknown amount of money and no screen
will show you which 300; `58/165`, `058/165` and `#58` are one card, because a
price source keyed on `058/165` finds nothing for `#58`; a graded slab carries
`PSA 10` as 100 tenths, because the grade *is* the price.

## Running one

```bash
node tools/claude-shim.mjs &          # inference on the subscription, not the API
cd components && cargo test -p card-identify   # watch it fail first, on purpose

reconciler/target/release/comp-goalrun \
  --checkout . --repo markkovari/holon --base main \
  --llm-base-url http://127.0.0.1:8787 \
  --github-token ~/.comp-secrets/ghpat --branches 3 --rounds 2
```

The shim now carries **images** as well as text — a base64 image block is written
to a temp file and `claude -p` is told to read it, with `Read` allowed for exactly
those requests. That is what lets a vision capability be developed and gated on the
subscription instead of the metered API.

## What is deliberately not built here

- **The WIT.** All three are plain Rust crates for now, the same shape as
  `components/semver-range`. Wrapping a green core as a component is mechanical and
  this repo does it constantly; wrapping a red one produces a contract nobody has
  satisfied and a catalogue entry that lies. The WIT lands with the implementation.
- **The providers.** A real `pokemontcg.io` fetcher and a real vision call are
  both egress-plus-a-key, and both are untestable offline — so the deterministic
  halves go first and the providers plug in behind them, the way
  `llm-inference` (the mock) and `anthropic-provider` (the real one) already sit
  behind one interface.
- **The domain app.** `binder-domain` — routes, the SPA, the marketplace — is one
  goal per slice once these three are green, and its gate is an `e2e-binder`
  recipe of the shape every other showcase already has.
- **A second game.** Magic and Yu-Gi-Oh! have different numbering, different
  grading conventions and different sources. Nothing above forbids them; nothing
  above pays for them either.
