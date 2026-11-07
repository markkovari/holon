# `luhn:checksum` (MoonBit)

Luhn (mod-10) validation and generation over a string of ASCII digits.
Exports `luhn:checksum/checksum@0.1.0` (`wit/luhn.wit`).

## Build & Test

```bash
cargo xtask build                # builds into components/target/wasm32-wasip2/release/luhn_checksum.wasm
cargo xtask parity luhn-checksum # verifies contract and parity
(cd components/luhn-checksum && moon test)
```

## Binary Characteristics

- **Size**: **9.4 KB** (vs 56 KB in Rust, an 83% reduction)
- **Toolchain**: `moon build --target wasm --release`, embedded via `wasm-tools --encoding utf16`.
- **Imports**: Pure compute — zero host imports, zero unsafe code.
