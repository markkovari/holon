# jco-vet-clinic — a full-stack app built only from comp components

A small **veterinary-clinic** app — browser frontend + HTTP backend — assembled
entirely from the `comp/` capability components running **in-process via jco**.
There is no business-logic crate: every cross-cutting concern (auth, RBAC,
sessions, audit, search, validation, notifications) is an unmodified comp
component, and the domain (pets, appointments, visit notes) is thin JS glue over
the same in-memory key-value store the components share.

Three roles:

| Role | Can |
|---|---|
| **pet-owner** | register/login, add + search **their own** pets, book appointments |
| **doctor** | see appointments, write **visit notes** (`notes:write`) |
| **admin** | assign roles, read the **audit log** (`*:*`) |

## Components used (all unmodified)

| Feature | Component | How it's included |
|---|---|---|
| accounts, login, 3-role **RBAC**, sessions, **audit** | **auth-guard** (composed with rate-limiter + audit-log) | `just compose` → `auth_guard.composed.wasm`, transpiled |
| pet full-text **search** | **search-index** | transpiled |
| request-body **validation** | **validate** | transpiled |
| appointment **notifications** | **notify-dispatch** | transpiled (config points at a local sink) |
| runtime **config** | **config-store** | transpiled |
| server-side **sessions** | **session-store** | transpiled (available to the auth layer) |
| pet **photo** upload validation (type/size) + signed ticket | **upload-policy** | transpiled |
| pet **photo** object storage | **blob-store** | transpiled |

Pet photos: an owner uploads an image (`POST /pets/:id/photo`, raw bytes);
**upload-policy** validates the content-type + size against the config policy and
mints/redeems a signed ticket, then **blob-store** holds the bytes (durable in the
same shared KV). `GET /pets/:id/photo` serves it back. Eight components total.

This is the **hybrid composition** pattern: the auth/RBAC/audit half is one
`wac`-composed wasm; the rest are transpiled separately and wired in TypeScript.
All six share **one** `wasi:keyvalue` store (the shim in `src/shims/keyvalue.js`),
so a pet the backend writes under `pet_*` is indexed by search-index and sits
beside the sessions and audit events auth-guard writes — one process, one Map.

## Run

```bash
npm install
npm start          # transpiles all 6 wasms, boots Fastify on :3000
# open http://localhost:3000  — log in with a demo account (printed on boot):
#   pet-owner  owner@acme-vet.test   / ownerpass1
#   doctor     doctor@acme-vet.test  / doctorpass1
#   admin      admin@acme-vet.test   / adminpass1
```

The SPA shows a different panel per role based on the principal returned by
`GET /auth/me`.

### Frontend (React + shadcn/ui)

The UI is a **Vite + React + TypeScript + Tailwind v4 + shadcn/ui** app in
`frontend/`, built to `public/` (which the Fastify backend serves). The built
bundle is committed — same convention as the `.wasm` files — so `npm start`
runs the whole stack with no extra build. To change the UI:

```bash
npm run build:frontend   # cd frontend && npm install && npm run build  -> ../public
# or, live-reload dev (proxies /auth,/pets,/appointments,/admin to :3000):
npm start &              # backend on :3000
npm run dev:frontend     # Vite dev server
```

shadcn components used: Card, Tabs, Input, Label, Button, Select, Table, Badge,
Sonner (toasts). Three role views (`owner-view`, `doctor-view`, `admin-view`) +
an `auth-card` (login/register tabs with a role picker), all calling the backend
over relative `fetch` paths with the bearer token from localStorage.

## Test

```bash
npm test           # e2e via Fastify app.inject (no network) — 8 cases
```

The suite walks the full stack: seed 3 roles → register an owner → add a pet
(validated + indexed) → search finds it → book an appointment (notify fires) →
**owner is 403 on `notes:write`** → doctor writes the note → admin reads the
audit trail → **owner is 403 on the admin route**. Every `authorize` decision is
recorded by the composed audit-log (visible as the JSON lines in test output).

## Backend routes

| Method + path | Guard (`authorize`) | Component path |
|---|---|---|
| `POST /auth/register` | — | `accounts.register` + `rbac.assignRole` |
| `POST /auth/login` | — | `accounts.login` |
| `GET /auth/me` | — | `authorizer.introspect` |
| `POST /auth/logout` | — | `session.revoke` |
| `GET /pets[?q=]` | `pets:read` | `search-index.query` / KV scan (owners see own) |
| `POST /pets` | `pets:write` | `validate.validate` → KV → `search-index.indexDoc` |
| `GET /appointments` | `appointments:read` | KV scan, role-filtered |
| `POST /appointments` | `appointments:write` | `validate` → KV → `notify-dispatch.send` |
| `POST /appointments/:id/notes` | `notes:write` | KV (doctor only) |
| `GET /admin/audit` | `audit:read` | reads audit-log's `al_*` keys |
| `POST /admin/assign-role` | `rbac:admin` | `rbac.assignRole` |

## Notes

- **No new Rust / no new WIT.** The only new code is JS/TS glue + the SPA.
- The composed auth-guard wires `audit:log/recorder` internally but does not
  re-export the audit `query` interface, so the admin view reads the raw `al_*`
  audit keys the recorder wrote to the shared store.
- `notify-dispatch` is configured against a local sink, so booking an
  appointment exercises the dispatch path without a live email vendor.
- In-process jco only (like `jco-embed`); the same `.wasm` bytes would run on
  wasmCloud behind an http-server provider in production.
