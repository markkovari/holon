# Embed qr:encode in-process via jco

The `qr:encode` component running **inside the Node process** — no wasmCloud, no
NATS, no host shims. Pure compute: text in, a scannable code out. `jco transpile`
turns `qr.wasm` into JS; this example calls its exported `encoder` interface
directly.

Three renderings, one `level` (`"low" | "medium" | "quartile" | "high"` error
correction):

- `svg(data, level, quietZone)` — a self-contained `<svg>…</svg>` string that
  scales to any size (viewBox + a single dark `<path>`). Embed it straight in
  HTML. `quietZone` is the margin in modules.
- `unicode(data, level)` — compact block characters (two module-rows per line)
  for a terminal.
- `matrix(data, level)` — the raw grid as JSON `{size, modules: [[bool,…],…]}`
  (true = dark) for callers that render it themselves.

Too much data for any QR version at the chosen level throws `too-long`.

Concretely this fills the `authgate` gap: TOTP enrollment should show the
scannable `otpauth://` QR every authenticator app expects, not a secret to type
by hand. The Reed-Solomon + masking is the vetted `qrcode` crate; this component
renders its module grid.

```
qr.wasm                  # the built component (pure compute, standard WASI only)
test/
  qr.test.ts             # svg / unicode / matrix + quiet-zone + too-long
gen/                     # transpile output (gitignored) -> gen/qr.js
```

## Run

```bash
npm install
npm run transpile        # qr.wasm -> gen/
npm test
```

`jco transpile qr.wasm -o gen` — no `--map` flags; the component imports only
standard WASI interfaces and computes in-process.
