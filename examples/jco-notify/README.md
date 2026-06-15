# Embed notify-dispatch in-process via jco

The `notify:dispatch` component transpiled with jco. It sends notifications over
outbound HTTP (`webhook` POSTs the body to a target; `email`/`sms` POST a JSON
envelope to a configured gateway URL — no vendor named in the contract).

```
notify_dispatch.wasm  # the built component
src/
  config-shim.js       # host shim for wasi:config/runtime (notify:email-url / notify:sms-url)
test/
  notify.test.ts       # config-gated channel routing (no live HTTP — see below)
gen/                   # produced by `jco transpile` (gitignored)
```

## Run

```bash
npm install
npm run transpile      # notify_dispatch.wasm -> gen/
npm test
```

## Why the in-process test does NOT do a live send

The component performs a **blocking** outbound request (`wasi:http/outgoing-handler`
+ a WASI pollable `.block()`). jco's preview2-shim backs outbound HTTP with async
`fetch`; in Node's single-threaded event loop, blocking the guest while `fetch`
needs that same loop to make progress deadlocks. So a live delivery can't be
driven in in-process jco.

The **HTTP delivery path is validated under wasmCloud** instead, where a real
`http-client` capability provider satisfies `wasi:http/outgoing-handler` (see
`infra/wadm.yaml`). What the in-process test covers is the synchronous,
network-free branch: with no gateway URL configured, `email`/`sms` return
`unsupported-channel` before any request — proving the component loads, reads
config, and routes channels correctly.

Gateway URLs come from config (`notify:email-url`, `notify:sms-url`); the shim
sources them from `NOTIFY_EMAIL_URL` / `NOTIFY_SMS_URL` env so a deployment (or a
wasmCloud `config:` block) can point them at any provider.
