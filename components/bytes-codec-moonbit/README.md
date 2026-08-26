# `bytes:codec` in MoonBit — a polyglot attempt, and where it stops

The same `bytes:codec` WIT the Rust component exports, implemented in MoonBit. It is
kept because it got further than "it does not work" and the place it stops is a
toolchain bug worth being able to re-test, not a design problem.

```bash
moon build --target wasm --release
wasm-tools component embed ../bytes-codec/wit _build/wasm/release/build/gen/gen.wasm \
  --world bytes-codec -o embedded.wasm
wasm-tools component new embedded.wasm -o bytes_codec_moonbit.wasm
```

## What worked

- `wit-bindgen moonbit` generated the whole canonical-ABI layer from the same
  `.wit` the Rust component uses, doc comments and all. The implementation is
  ordinary MoonBit: no `unsafe`, no pointers, no C.
- It builds to a **17 KB component** against the Rust build's 63 KB, and
  `wasm-tools component wit` confirms it exports `bytes:codec/codec@0.1.0` — a real,
  interchangeable-by-shape artifact.
- Dropping it in place of the Rust `.wasm` and running `just gate-codec` composes and
  serves it without touching `codec-probe` or the gate. That part of the premise
  holds: **the gate does not care what compiled the thing it judges.**

## Where it stops

Strings cross the boundary as UTF-16 into a UTF-8 slot:

    to-hex(deadbeef)   expected "deadbeef"   got "d\0e\0a\0d\0"
    encode(foo)        expected "Zm9v"       got "Z\0m\0"

A MoonBit string is UTF-16 internally. `wit-bindgen` 0.61.0's MoonBit generator
lowers it into the canonical ABI's UTF-8 string slot without transcoding, and reports
the code-unit count as the byte length — so the receiver reads exactly half of it,
NUL-interleaved. Every export here returns or takes a string, so all thirteen cases
fail, and they fail identically.

The MoonBit code is not the problem: the same arithmetic passes on `moon test` for
both the native and the wasm backends (`'A'.to_int()` is 65, a string indexes to the
code unit you expect). What is wrong is the lowering, and it is one layer below
anything in this repository.

## What this proved anyway

The **gate caught it in seconds, and precisely** — an ABI-level encoding bug, from a
shell script, with no debugger. A unit test in the implementation's own language
could not have: MoonBit's own test runner passes on the same functions. That is the
argument for putting a specification at the WIT boundary rather than in one
language's test framework, and it is worth more than the rewrite would have been.

## Retrying

Rebuild and re-run `just gate-codec` with this artifact swapped in. If the string
lowering has been fixed upstream, the thirteen cases pass unchanged — nothing here or
in the gate needs editing.
