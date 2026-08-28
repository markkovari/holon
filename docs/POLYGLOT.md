# The same app, five compilers

The binder (`docs/apps/BINDER.md`) composes three capabilities. Two of them are pure
arithmetic, and those two are implemented here in **Rust, C, Go, JavaScript and
Python** — dropped into the same composition, judged by the same unedited e2e.

```bash
just e2e-binder-poly c  portfolio-value
just e2e-binder-poly py price-history
just e2e-binder-poly-all          # every row in the table below
```

Each run builds the component, swaps it in for the Rust build **by filename**,
derives the composition from `binder-domain`'s own imports (ADR-0087), runs
`examples/binder/tests/binder.rs` — 122 assertions, none of them edited — and puts
the Rust build back on the way out.

The claim is not "these toolchains emit components". It is that **the artifact
boundary is the real boundary** (ADR-0086, ADR-0095): a composition does not know
what compiled its parts, and a specification written at the WIT boundary judges
whichever artifact satisfies the contract.

## What actually happened

| capability | language | toolchain | artifact | WASI imports | e2e |
|---|---|---|--:|--:|---|
| `portfolio:value` | **C** | `wit-bindgen c` + wasi-sdk 25 | **60 KB** | **0** | pass |
| `portfolio:value` | Rust | `cargo component` | 77 KB | 15 | pass |
| `portfolio:value` | Go | `wit-bindgen go` + Go 1.27 + adapter | 2,539 KB | 18 | pass |
| `portfolio:value` | C# | `wit-bindgen csharp` + .NET 10 | — | — | **does not build** |
| `price:history` | Rust | `cargo component` | 60 KB | 14 | pass |
| `price:history` | Go | `wit-bindgen go` + Go 1.27 + adapter | 2,497 KB | 18 | pass |
| `price:history` | JavaScript | `componentize-js` | 12,239 KB | 18 | pass |
| `price:history` | Python | `componentize-py` | 17,911 KB | 25 | pass |

Both WIT worlds import **nothing**. They are pure compute: numbers in, numbers out.

## The finding: a runtime is a boundary

The interesting column is not the size. It is that **every language with a runtime
drags WASI in, and that includes the Rust this repository ships**.

`components/portfolio-value` — a FIFO cost-basis calculator that reads no clock,
opens no file and prints nothing — imports fifteen interfaces:

```
wasi:cli/stdout, stderr, stdin, environment, exit,
wasi:cli/terminal-{input,output,stdin,stdout,stderr},
wasi:io/{poll,error,streams}, wasi:clocks/monotonic-clock,
wasi:random/insecure-seed
```

That is Rust's `std` — panic formatting, the environment, the exit path — not
anything the component does. ADR-0023 says isolation is a linker boundary; a
capability that can reach a clock and five terminal interfaces has a wider boundary
than its own WIT says it does, in every language here except one.

**C is the exception, and only with `-mexec-model=reactor`.** Built as the default
command it exports `wasi:cli/run` and imports an exit, an environment and stderr for
a `main` that is never called. As a reactor the import section is empty — which is
what the world actually declares.

So the honest ordering is: C gets what the WIT promised, and every other language
here — Rust included — ships a runtime that quietly widens the boundary. That is
worth an ADR of its own, and it is not a Go problem.

JavaScript's list is the one to look at twice: it includes
`wasi:http/outgoing-handler`. A price calculator that can make outbound HTTP calls.

## Per-language notes

### C — `components/portfolio-value-c`

The only one needing no post-processing: wasi-sdk's clang targets `wasm32-wasip2`
and its `wasm-component-ld` emits a component directly. Also the only one that hits
the canonical ABI by hand — returned strings must be `malloc`'d because the runtime
frees them after lifting, so `string_dup` is right and `string_set` is a
use-after-free waiting to happen.

Needs wasi-sdk (~200 MB), which is not vendored.

### Rust — `components/portfolio-value`, `components/price-history`

The baseline, and the default. Small, fast to build, and — see above — not as clean
at the boundary as this repository has been assuming.

### Go — `components/portfolio-value-go`, `components/price-history-go`

Three things had to be worked out, all confined to `tools/build-polyglot.sh`:

- **TinyGo cannot build these bindings.** It targets wasip2 natively, needs no
  adapter, and would have been far smaller — but `wit-bindgen`'s Go output depends
  on `runtime.Pinner` to hold the canonical ABI's return area, and TinyGo has none
  (`undefined: runtime.Pinner`). So: standard Go, wasip1, and an adapter.
- **`-ldflags=-checklinkname=0`**, because that same runtime reaches `runtime.sbrk`
  by `//go:linkname` and Go 1.23 made it an error.
- **The adapter has a vintage.** The one vendored under `bench/node_modules` with
  jco is older than our `wasm-tools` and is rejected outright
  (`adapter module did not export adapter_monotonic_clock_set_paused`). A current
  one is fetched once into `components/target/`.

### JavaScript — `components/price-history-js`

The only one that needed nothing installed: `componentize-js` was already vendored.
SpiderMonkey ends up inside the artifact, which is the 12 MB. `u64` crosses as
`BigInt` and mixing one with a `Number` throws rather than coercing — a mercy, since
a u64 timestamp rounded through a double is a bug that surfaces years later.

### Python — `components/price-history-py`

The odd one out: `componentize-py` does not use `wit-bindgen` at all. It has its own
generator and ships a CPython interpreter plus the module's bytecode, so what runs
in the composition is an interpreter.

**It exposed a real bug in our own tooling.** `comp-plug` refused it:

```
price-history: constant expression required: non-constant operator: i32.add
```

componentize-py's CPython uses **extended constant expressions**, a shipped Wasm
feature. `comp-plug` and `components/wit-reflect` were pinned to `wac-graph 0.6`,
whose bundled `wasmparser` predates it. Upstream `wac` 0.10.1 composes the same
artifact without complaint. The bump to `wac-graph 0.10` needed **no source
changes** in either crate; every component still builds and the reconciler suite
is unchanged.

That is what a gate at the artifact boundary is for: a language nobody here uses
found a two-year-old pin in the composer.

### C# — `components/portfolio-value-cs`, kept as a reproduction

`wit-bindgen csharp --runtime mono` generates correctly and the implementation
compiles with zero warnings. The link then fails:

```
failed to encode component
  2: failed to find export of interface `portfolio:value/valuation@0.1.0` function `value-at`
```

mono never emits the native-to-managed thunks for the generated
`[UnmanagedCallersOnly]` exports — the entry point name appears nowhere in the
generated C. Tried: `_IsLibraryMode=true` (the flag that gates thunk generation in
`WasmApp.Common.targets:779`), `RunAOTCompilation=true`, `OutputType=Library` (which
ILLink rejects for having no entry point). Two further obstacles found along the
way, both fixed and both worth knowing:

- .NET 10's wasi pack requires **exactly wasi-sdk 25.0** and refuses 27.
- It hard-codes its own world into the link
  (`WasiHttpWorld_component_type.wit`, `WasiApp.targets:417`) with no property to
  override, so the artifact is a `wasi:cli/run` command and our export is nowhere in
  it. Passing a second `--component-type` via `LinkerArg` does merge the worlds —
  that part works.

The remaining gap is `wit-bindgen`'s `mono` runtime mode against .NET 10. The other
mode, `--runtime native-aot`, needs NativeAOT-LLVM from an experimental feed and was
not attempted.

## What is deliberately still Rust

`card:identify`, the binder's third capability. It parses a fenced model answer into
typed fields and calls the vision model over `wasi:http`; hand-rolling JSON and HTTP
in four more languages is noise, not proof.

## How the numbers were checked

Not by trusting a green test. Changing the Go FIFO to consume the *newest* lot
instead of the oldest fails the app:

```
assertion `left == right` failed: average cost would say 1000
  left: Number(-1000)
 right: 2000
```

The same control found a hole in the e2e: mutating the *other* FIFO branch — a
disposal that empties a lot exactly — changes nothing the binder scenario observes.
That path is composed but unjudged. `components/portfolio-value/tests/valuation.rs`
covers it against the Rust functions; nothing covers it at the boundary.
