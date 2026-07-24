# Embed diff:text in-process via jco

The `diff:text` component running **inside the Node process** — no wasmCloud, no
NATS, no host shims. Pure compute: text in, edits / patch / result out. `jco
transpile` turns `textdiff.wasm` into JS; this example calls its exported
`differ` interface directly.

Line-based diffing in two shapes plus apply:

- `diffLines(a, b)` — a line-level **edit script**: a list of `{ tag:
  "equal" | "insert" | "delete", val }` ops (an LCS backtrack). For rendering an
  inline diff in a UI.
- `unified(a, b, fromLabel, toLabel, context)` — a standard **unified diff**
  string with `context` lines around each change and `---`/`+++` labels. Empty
  string when the texts are identical. For storing or transmitting a patch.
- `applyUnified(a, patch)` — apply a unified diff to `a`, returning the patched
  text. Verifies every context/delete line against the source; a mismatch throws
  a typed `context-mismatch`, a bad header throws `malformed-patch`.

The headline is the round-trip property: **`applyUnified(a, unified(a, b)) ===
b`** — checked over many edit shapes × context sizes.

Useful for history/review views (track, bin), and the piece the collaborative
editor (`scribe`) uses to show per-edit changes.

```
textdiff.wasm            # the built component (pure compute, standard WASI only)
test/
  textdiff.test.ts       # edit script, unified output, round-trip, errors
gen/                     # transpile output (gitignored) -> gen/textdiff.js
```

## Run

```bash
npm install
npm run transpile        # textdiff.wasm -> gen/
npm test                 # behavioral + round-trip checks
```

`jco transpile textdiff.wasm -o gen` — no `--map` flags; the component imports
only standard WASI interfaces and computes in-process.
