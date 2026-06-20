# jco-id

Exercises the `id:generate@0.1.0` component in-process via [jco](https://github.com/bytecodealliance/jco).
The component is transpiled to JS and its `generator` interface is called
directly from a Node test — no host, no network.

## What it generates

- **ULID** — 26-char, Crockford base32, **time-prefixed and lexicographically
  sortable**. The first 10 chars encode the millisecond timestamp, the last 16
  are random.
- **UUIDv4** — random RFC 4122 v4 identifier.
- **nanoid** — compact, URL-safe id (`[A-Za-z0-9_-]`), caller-chosen length.
- **short-code** — human-friendly code over an *unambiguous* alphabet (no
  `0 O 1 I L U`), nice for invite/coupon codes read aloud or typed.

## Why sortable ids matter

A ULID sorts the same way by string as by creation time. That means inserts
land at the end of an index (no random-write page splits), range scans by time
are trivial, and a key/value store keeps related records naturally adjacent —
all the locality of an auto-increment id without a central sequence, plus the
uniqueness of a UUID.

## Run

No shim is required: the component only imports `wasi:clocks/wall-clock` and
`wasi:random/random`, both of which jco auto-shims in its transpile output.

```bash
npm install
npm test
```

`npm test` runs `jco transpile id_generate.wasm -o gen` first, then runs the
Node test against the generated `gen/id_generate.js`.
