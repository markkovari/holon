# bin — a paste / gist bin over a pure-compute pipeline

A **paste bin**: drop in some Markdown, and everything that shapes the paste is a
**stateless transform contract** chained in a row — the input is validated, PII
in the body is **masked before it's stored**, the title becomes a de-duplicated
URL slug, and on read the Markdown renders to sanitized HTML. Chosen because
it's the one axis none of the other showcases lead with: an app that is almost
entirely a **fold over pure functions**, with exactly one stateful step (the
store). It's the clearest demonstration that "the domain is composition."

Same shape as the other showcases: one **`paste-bin`** HTTP component that
exports `wasi:http` and imports only WIT contracts. Four of the five are
pure-compute (`validate:schema`, `pii:redact`, `md:render`, `slug:generate`) —
no host state of their own — and only `records:store` persists anything.

![The paste bin: pasting Markdown containing an email and a card number stores it with the PII masked (a "2 PII masked" badge), renders the Markdown to safe HTML with a raw &lt;script&gt; escaped, and gives a duplicate title a distinct slug — all over one composed wasm component](../media/paste.gif)

## Why it's almost pure composition

| paste concern | contract | pure? | how |
|---|---|:--:|---|
| input validation | `validate:schema` | ✓ | body required + length-bounded before anything else runs |
| PII masking at ingest | `pii:redact` | ✓ | `detect` counts findings, `mask` rewrites emails / cards / SSNs / phones — **before** the write |
| URL slug | `slug:generate` | ✓ | `slugify(title)` + `uniquify(base, taken)` — de-duplicated against existing slugs |
| Markdown → HTML on read | `md:render` | ✓ | `to-html` escapes raw HTML (no XSS) + sanitizes link schemes; `to-text` for the preview |
| the stored paste | `records:store` | ✗ | the one stateful step — and it only ever sees the already-redacted body |

The domain logic is a literal pipeline: `validate → redact → store → slug`, then
`render` on the way out. There is no bespoke business logic — just the order of
the transforms.

## The new axis

The other showcases lead with state, streams, or timers. Bin leads with
**transforms**:

- **pure-compute chain** — four of five contracts are pure functions. The app's
  behavior is the *composition order*, not code inside the domain component. Swap
  `pii:redact` out and the paste stores raw; swap `md:render` for a different
  renderer and nothing else changes.
- **redact-before-store** — the security property is positional: masking runs at
  **ingest**, so the raw email/card **never reaches the record store** (the
  `/api/raw` view proves it). Redaction is not a display-time filter you can
  forget — it's baked into the write path.

## Product surface (one component, anonymous)

```
POST /api/paste        {title?, body, syntax?}   validate → redact → store → slug
GET  /api/paste/{id}                              metadata + rendered HTML + text preview
GET  /api/pastes                                  recent pastes (metadata)
GET  /api/raw/{id}                                the stored (redacted) body, text/plain
GET  /                                            usage
```

All routes under `/api/…` so the static-dir SPA fallback doesn't shadow them.
No SSE — the flow is request/response.

## Domain model (`records:store`)

- **paste** — `{id, slug, title, body (redacted), syntax, redacted: n, at}`. The
  `body` stored is **always** the masked version; `redacted` is the count of PII
  findings removed at ingest. The slug is `slugify(title)` made unique against
  existing slugs.

## Component map

**Reused as-is (5):** `validate:schema` (input), `pii:redact` (mask at ingest),
`md:render` (safe HTML), `slug:generate` (URL slug) — all pure compute — plus
`records:store` (the one stateful piece). Plus host WASI:
`wasi:clocks/wall-clock` (`at`), `wasi:keyvalue` (the store's backend). This
showcase is the app that leans hardest on the pure-compute utilities.

**New (1):** `paste-bin` — `bin:app` exports `wasi:http`. The transform pipeline
+ the read-side render.

**Not used:** `auth-guard` (anonymous bin; a real deploy would rate-limit with
`ratelimit:guard`), and anything stateful beyond the single record store.

## Build order (each rung is demoable)

1. **Validate + store** — `POST /api/paste` over `validate:schema` +
   `records:store`. `just e2e-paste` proves an empty body is rejected.
2. **Redact at ingest** — `pii:redact` masks the body **before** the write; e2e
   proves the raw email + card never appear in `/api/raw/{id}`.
3. **Render + slug + browser UI** — `md:render` escapes a raw `<script>` while
   rendering real Markdown; `slug:generate` gives duplicate titles distinct
   slugs; a two-pane SPA (paste ⇄ rendered) shows the "N PII masked" badge.
   `just host-paste`, paste something with an email in it.
4. **Bench** — the pure-compute dimension: transforms-per-second through the full
   validate→redact→render chain, showing near-zero host overhead. See
   `bench/PASTE-BENCH.md`.

## Non-goals (v1)

Syntax highlighting (the `syntax` field is stored but not colorized), paste
expiry / visibility (public only), edit/versioning, and account ownership. The
showcase demonstrates the **pure-compute transform composition**, not a
full Gist clone.

> Note: because masked values use `*`, a masked token inside Markdown can pick
> up emphasis styling on render (e.g. `*` reads as italic). The stored body is
> correctly redacted regardless — this is cosmetic to `md:render`, not a leak.
