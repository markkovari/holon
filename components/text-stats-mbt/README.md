# text-stats (MoonBit)

An agent tool as a component: `holon:tool/tool.call` (`wit/tool/tool.wit`) for an agent-runtime
connector's `wrpc`, and `wasi:http/handler` (p3) for its `url`. Both take a JSON object of arguments
and answer JSON, or an error the model reads.

```bash
tools/build-polyglot.sh mbt text-stats   # -> components/target/text_stats_mbt.wasm, about 0.3 s
(cd components/text-stats-mbt && moon test --target wasm)
```

Needs `moon` (`~/.moon/bin`, or `MOON=`) and `wasm-tools`.

## A new tool

Copy this directory to `components/<name>-mbt`, rename the world in `wit/world.wit` and the
`holon/text-stats` prefix in `moon.mod` and every `moon.pkg`, and write `tool/tool.mbt`: one
`pub fn call(args : Json) -> Result[Json, String]`. Nothing else changes.

| File | Who writes it |
|---|---|
| `tool/tool.mbt` | the tool's author |
| `tool/json.mbt`, `gen/**/impl.mbt`, `gen/**/moon.pkg` | fixed, by hand |
| everything else | `wit-bindgen moonbit` 0.63 (0.58 panics on the p3 world) |

Regenerating the bindings rewrites `gen/**/moon.pkg`; put the hand-written ones back after it.

## Strings

A MoonBit `String` is UTF-16, so the component is embedded with `--encoding utf16`, and:

- `String::to_bytes()` is UTF-16LE. For UTF-8 use `@tool.utf8`.
- Cutting by `length()` can split an emoji, and an unpaired surrogate cannot leave the component:
  over wRPC the host refuses the result, and `@utf8.encode` aborts. `tool/json.mbt` repairs whatever
  a tool returns (U+FFFD for a half character), so a slip costs one character, not the call.
