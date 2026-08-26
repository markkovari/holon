# `bytes:codec` in JavaScript — the polyglot proof that passes

The same `bytes:codec` WIT the Rust component exports, implemented in JavaScript and
componentized with `jco`. **It passes all thirteen cases of
`components/bytes-codec/gate.sh` with nothing in the gate or the probe edited.**

```bash
bench/node_modules/.bin/jco componentize components/bytes-codec-js/codec.js \
  --wit components/bytes-codec/wit/codec.wit --world-name bytes-codec \
  -o components/bytes-codec-js/bytes_codec_js.wasm

# then swap it in for the Rust build and run the contract against it
cp components/bytes-codec-js/bytes_codec_js.wasm \
   components/target/wasm32-wasip2/release/bytes_codec.wasm
just gate-codec
```

## What it proves

A capability in this pool can be written in a language nothing else here uses, and be
judged by the specification that already exists. The gate is at the WIT boundary, so
it does not care what compiled the thing it is judging — which is also what will let
it judge an artifact fetched by digest and never built here at all.

`jco` was already vendored: forty-six `examples/jco-*` use it to TRANSPILE Rust-built
components and run them in Node. None of them produced one. `componentize-js` was
sitting in `node_modules` the whole time as a transitive dependency of a tool the repo
uses for the opposite direction.

## What it costs, and why it is not the default

| implementation | artifact | verdict |
|---|--:|---|
| Rust | 63 KB | passes — the one that composes |
| MoonBit | 18 KB | fails on a `wit-bindgen` string-lowering bug (see `../bytes-codec-moonbit`) |
| **JavaScript** | **12 MB** | **passes** |

12 MB, because SpiderMonkey is inside the artifact. That is ~190× the Rust build for
the same four functions, and against ADR-0019's measured 2.3 MiB per extra component
in a host it is the one language choice here that does move the number.

It also stops being pure compute: the componentized artifact imports `wasi:io`,
`wasi:cli` and a clock, which the Rust and MoonBit builds do not. `comp-host` provides
them, so it composes and runs — but a world that declared no imports now has eight.

So: the right tool for a capability that is genuinely easier to express in JavaScript,
or one whose author only writes JavaScript. The wrong tool for four functions of byte
arithmetic, which is why `bytes-codec` ships as Rust.
