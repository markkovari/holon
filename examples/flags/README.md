# flags — live feature rollout, no redeploy

A rollout console for the **feature-flag** capability (see [`docs/apps/FLAGS.md`](../../docs/apps/FLAGS.md)):
add a flag, drag a percentage slider, or trip a kill-switch — and every open
window updates *instantly* over a held-open **SSE** stream, with each affected
subject **sticky** (the same tiles light every evaluation — real cohorts, not
`rand()` flicker).

One composed wasm HTTP component (`flags-domain` + `feature-flags` + `event-bus`
+ `id-generate`) on the native Rust host. The evaluation, runtime rules, and
stable-hash bucketing are entirely the `featureflags:guard` contract; the domain
just exposes it and streams each change.

## Run it

```bash
just host-flags             # compose + serve on http://127.0.0.1:3017
```

Open the page:

1. **Add flag** — it starts at 0% (all 100 tiles dark).
2. Drag its slider to ~30% — about 30 tiles light **instantly**. Nudge to 60%:
   ~30 more join and **none already-on turn off** (sticky, monotone cohorts).
3. Hit **Kill** — every tile goes dark at once (off beats any percentage).
4. Open a second window: a change in one repaints the grid in the other live.

## Test it

```bash
just e2e-flags              # set+eval, stickiness + monotone cohorts, live SSE
```

The e2e asserts a subject doesn't flicker between evals, that raising the
percentage only ever *adds* subjects, that the kill-switch darkens all, and that
a rule flip reaches a separate held-open SSE connection.
