# Polyglot WASM Components

This directory isolates non-Rust language reproductions and verification suites demonstrating WASI preview 2 and WIT component model interoperability across diverse languages:

- `bytes-codec-js` (JavaScript / ComponentizeJS)
- `bytes-codec-moonbit` (MoonBit)
- `portfolio-value-c` (C / clang)
- `portfolio-value-cs` (C# / NativeAOT-LLVM)
- `portfolio-value-go` (Go / TinyGo)
- `price-history-go` (Go / TinyGo)
- `price-history-js` (JavaScript)
- `price-history-py` (Python / componentize-py)

Separating polyglot proof artifacts from production Rust capability components keeps the top-level `components/` catalog fast, clean, and directly aligned with the Cargo workspace (ADR-0128).
