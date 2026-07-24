# Embed cron:expr in-process via jco

The `cron:expr` component running **inside the Node process** — no wasmCloud, no
NATS, no host shims. Pure compute: expression + time in, times out. `jco
transpile` turns `cron.wasm` into JS; this example calls its exported `parser`
interface directly.

Turns a cron string into actual fire times (all UTC):

- `parse(expr)` — validate + **normalize** (expand `@daily`/`@hourly`/… macros,
  lower `jan`/`mon` names and `*/n` steps to numbers). Throws
  `invalid-expression` on a bad expression, `unsatisfiable` if it never fires.
- `matches(expr, unix)` — does the schedule fire at that Unix second? (Minute
  granularity — cron ignores seconds.)
- `next(expr, after, count)` — the next `count` fire times strictly after
  `after`, oldest first (Unix seconds; a `BigUint64Array` in JS).

Supports the standard 5 fields (`min hour dom month dow`) with `*`, `,`, `-`,
`/`, 3-letter names, and `@yearly`/`@monthly`/`@weekly`/`@daily`/`@hourly`.
Day-of-month vs day-of-week follow Vixie cron (both restricted ⇒ either matches).

The layer `sched:timer` is missing: it schedules a callback, but nothing else
turns `"0 */6 * * *"` into the timestamps to schedule.

```
cron.wasm                # the built component (pure compute, standard WASI only)
test/
  cron.test.ts           # parse/normalize, matches, next (incl. a leap-day jump)
gen/                     # transpile output (gitignored) -> gen/cron.js
```

## Run

```bash
npm install
npm run transpile        # cron.wasm -> gen/
npm test
```

`jco transpile cron.wasm -o gen` — no `--map` flags; the component imports only
standard WASI interfaces and computes in-process.
