# bin — a paste/gist bin (e2e)

The [docs/apps/PASTE.md](../../PASTE.md) showcase as one composed wasm HTTP component on
the native Rust host, plus a browser SPA. The pure-compute axis: the app is a
fold over four stateless transform contracts (validate → redact → render →
slug), with exactly one stateful step (the record store).

## Run it

```bash
just host-paste       # from repo root; bin on http://127.0.0.1:3024
```

Paste Markdown that contains an email or a card number, submit, and watch the
PII get **masked at ingest** (a "N PII masked" badge) and the Markdown render to
safe HTML on view (a raw `<script>` is escaped, not executed).

## Test it

```bash
just e2e-paste        # composes + builds host + runs tests/paste.rs
```

Proves: an empty body is rejected (`validate`); PII in the body is masked
**before** storage (the raw email/card never appears in `/api/raw`); Markdown
renders to sanitized HTML (a `<script>` is escaped); duplicate titles get
distinct slugs (`slug::uniquify`).

## What's composed

`paste-bin` (`bin:app`) imports only contracts — four pure-compute + one store:

- `validate:schema` — body required + length-bounded (pure)
- `pii:redact` — mask emails/cards/SSNs/phones at ingest (pure)
- `md:render` — safe Markdown → HTML on read (pure)
- `slug:generate` — URL-safe, de-duplicated slug (pure)
- `records:store` — the one stateful piece (stores the already-redacted body)

plus host WASI: `wasi:keyvalue`, `wasi:clocks`. No auth.
