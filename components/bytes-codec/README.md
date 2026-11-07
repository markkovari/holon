# `bytes:codec` (MoonBit)

The `bytes:codec` WASI Preview 2 capability, implemented in MoonBit.
Exports `bytes:codec/codec@0.1.0` (`wit/codec.wit`).

## Build & Test

```bash
cargo xtask build                # builds into components/target/wasm32-wasip2/release/bytes_codec.wasm
cargo xtask parity bytes-codec   # validates contract & gate parity against baseline
components/gates.sh              # executes RFC 4648 gate suites over composed codec-probe
```

## Binary Characteristics

- **Size**: **18 KB** (vs 64 KB in Rust, a 72% reduction)
- **Toolchain**: `moon build --target wasm --release`, embedded via `wasm-tools --encoding utf16`.
- **Imports**: Pure compute — zero host imports, zero unsafe code.

