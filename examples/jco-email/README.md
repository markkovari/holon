# jco-email — `email:template` in-process

Runs the `email:template` WASM component in plain Node via
[`jco`](https://github.com/bytecodealliance/jco) transpilation — no runtime,
no server. The component stores transactional email templates (subject + text +
html, each with `{name}` placeholders) and renders one against a set of
variables into a finished message ready to hand to a sender.

HTML placeholder values are **HTML-escaped** to prevent injection
(`<` → `&lt;`, `&` → `&amp;`, `"` → `&quot;`); the subject and text bodies are
left **raw**, since plain text needs no escaping.

The single non-standard import, `wasi:keyvalue/store`, is satisfied by
`src/keyvalue-shim.js` — a trivial in-memory `Map`. Swap it for redis / sqlite /
NATS to make it real; the component neither knows nor cares.

## Run

```bash
npm install
npm test
```

`npm test` first runs `jco transpile email_render.wasm -o gen` (mapping the
keyvalue import to the shim), then executes the `node:test` suite in
`test/email.test.ts`.
