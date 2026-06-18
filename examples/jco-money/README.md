# Embed money:amount in-process via jco

The `money:amount` component running **inside the Node process** — no wasmCloud,
no NATS. `jco transpile` turns `money.wasm` into JS; this example calls its
exported `arithmetic` interface directly.

Money is held as **exact integer minor units** (cents, pennies, yen) per
currency — never floats — so `10.99 + 0.01 === 11.00` always, with no rounding
drift. `allocate` splits a total across shares while distributing the leftover
minor units, so the parts always sum back to the original.

```
money.wasm              # the built component (copy of components/target/.../money.wasm)
test/
  money.test.ts         # parse/format, add/subtract, scale, allocate, errors, compare
gen/                    # transpile output (gitignored)
```

## Pure-compute: no host imports, no shims

Unlike the other examples, `money:amount` imports no WASI host functions — it is
pure arithmetic. So there is **nothing to `--map`** and **no shim** to write; the
transpiled JS runs standalone.

```bash
jco transpile money.wasm -o gen
```

## Run

```bash
npm install
npm run transpile         # money.wasm -> gen/
npm test                  # behavioral checks
```

## API notes

`units` and the `scale` factor are `bigint` (s64 minor units); `shares` is a
`number`; `compare` returns a `number` (-1 / 0 / 1). `parse` throws on an
unknown currency or bad format, and `add`/`subtract` throw a `currency-mismatch`
error when the operands' currencies differ.
