# drop — a presigned direct-upload drop-box

A **file drop-box**: pick a file and watch the backend do three things it never
mixes — **answer the policy question** (is this content-type allowed, under the
size cap?), hand back a **short-lived signed ticket**, then take the bytes the
client PUTs against that ticket and store them. Downloads go out under a
**signed, expiring link** that never exposes the store. Chosen because it's the
one axis none of the other showcases touch: a **presigned direct-upload** flow,
where the control path (authorize + sign) is deliberately separate from the data
path (the bytes), and a tampered or expired link is refused.

Same shape as the other showcases: one **`upload-drop`** HTTP component that
exports `wasi:http` and imports only WIT contracts. The policy decision is the
`upload:policy` contract, the bytes land in `blob:store`, the shareable link is
an HMAC from `webhook:sign` — no S3, no presigned-URL SDK, no bespoke crypto.

![The drop-box: choosing a file asks for a ticket (the policy answer, no bytes), an allowed type mints a signed ticket, the bytes PUT straight to storage, and a signed download link round-trips them — while a blocked type is refused at ticket time — all over one composed wasm component](docs/media/drop.gif)

## Why it's almost pure composition

| drop concern | contract | how |
|---|---|---|
| the policy answer + signed, expiring upload ticket | `upload:policy` | `authorize(tenant, content-type, size, ttl)` → signed `ticket`; `redeem(token)` → `grant{object-key, content-type, max-size}` (HMAC + expiry checked inside) |
| the object bytes | `blob:store` | `put(container, key, data, ct)` / `get` / `head` / `delete` — keyed by the store-minted metadata id |
| the shareable download link | `webhook:sign` | `sign(id\|exp, secret)` mints the link HMAC; `verify(...)` on download — a Stripe-style signature over the object id + expiry |
| object metadata (id, key, type, size, when) | `records:store` | the store mints the id; the blob is keyed under that same id so meta + bytes stay in lockstep |

The domain logic is a thin pipeline — authorize (no bytes) → redeem → store →
record; and on the way out, sign → verify → stream. Everything hard (ticket
HMAC, object storage, link signing) is the contract.

## The new axis

The others move data through a request or a stream. Drop splits **control from
data** and proves the split:

- **presigned** — `POST /api/tickets` touches **zero bytes**; it only answers
  *may this be stored?* and returns a signed ticket. A blocked content-type or
  an oversize request is refused **here**, before a single byte is uploaded.
- **signed links** — a download URL carries `?sig=&exp=`; the bytes stream only
  if the HMAC verifies and hasn't expired. Tamper with the signature and it's a
  403. This is the only showcase whose headline is *a capability URL that
  authorizes itself* — no session, no auth component.

## Product surface (one component, anonymous)

```
POST /api/tickets       {content-type, size}          policy check → {token, object-key, expires}
PUT  /api/blob/{token}  (raw body)                     redeem ticket → store bytes → {id, size}
GET  /api/objects                                      list stored object metadata
GET  /api/object/{id}                                  metadata + a freshly signed download link
GET  /api/blob/{id}     ?sig=&exp=                      verify signed link → stream bytes
GET  /api/stats                                        object count + total bytes
GET  /                                                 usage
```

All routes under `/api/…` so the static-dir SPA fallback doesn't shadow them
(same rule as search/pulse/pipeline/flags). No SSE — drop is request/response;
the *new* thing is the presigned control/data split, not a stream.

## Domain model (`records:store`)

- **object** — `{id, object_key, content_type, size, at}`. The record store
  mints the `id` on create; the blob is stored in the `drop` container under the
  **same** id, so `GET /api/object/{id}` hydrates metadata and the download path
  reads the matching bytes. If the blob write fails after the record is created,
  the orphaned record is deleted; if the record write fails after the blob
  lands, the blob is deleted — no half-objects.

## Component map

**Reused as-is (4):** `upload:policy` (the gate + ticket), `blob:store` (the
bytes), `webhook:sign` (the download-link HMAC), and `records:store` (object
metadata + the id that keys the blob). Plus host WASI: `wasi:clocks/wall-clock`
(ticket + link expiry, `at`). This showcase is the first to exercise
`upload:policy`, `blob:store`, and `webhook:sign` in one app.

**New (1):** `upload-drop` — `drop:app` exports `wasi:http`. The upload pipeline
(authorize → redeem → store → record) + the signed download path
(sign → verify → stream).

**Not used:** `auth-guard` (the ticket *is* the authorization — a signed
capability, not a session), and anything stream/SSE (this is the
request/response one).

## Build order (each rung is demoable)

1. **Ticket** — `POST /api/tickets` over `upload:policy`. `just e2e-drop` proves
   a blocked content-type and an oversize request are both refused at ticket
   time, with **no bytes uploaded**.
2. **Redeem + store** — `PUT /api/blob/{token}` redeems the ticket and stores to
   `blob:store`, keyed under the store-minted metadata id; e2e proves the bytes
   land and the object is listed.
3. **Signed download + browser UI** — `webhook:sign` mints an expiring link; the
   SPA shows the ticket → upload → sign flow live. e2e proves the link
   round-trips the exact bytes and a **tampered signature is refused (403)**.
   `just host-drop`, drop a file and watch the three steps.
4. **Bench** — the control/data split dimension: ticket-mint throughput (bytes
   never touched) vs upload throughput, and signature-verify overhead per
   download. See `bench/DROP-BENCH.md`.

## Non-goals (v1)

Multipart / resumable / chunked uploads (one PUT per object — the contract is a
single `put`), server-side transforms (thumbnailing, transcoding), and real
object-storage backends (the blob store is KV-backed for the demo). The showcase
demonstrates the **presigned composition**, not a production asset pipeline.
