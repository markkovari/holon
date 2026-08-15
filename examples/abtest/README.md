# abtest — live A/B/n experiments

A console for the **experiment** capability (see [`docs/apps/EXPERIMENT.md`](../../EXPERIMENT.md)):
define named variants with weights, watch 100 subjects split into arms
**stickily**, fire conversions, and see the **per-arm conversion rate** update
live over **SSE**.

One composed wasm HTTP component (`abtest-domain` + `experiment-assign` +
`metrics-collect` + `event-bus` + `id-generate`) on the native Rust host.
Assignment is entirely the `experiment:assign` contract; attribution is
`metrics:collect`; the domain composes them and streams each event.

## Run it

```bash
just host-abtest            # compose + serve on http://127.0.0.1:3018
```

Open the page:

1. **Weights** — drag `control` / `variant-a` / `variant-b`. The 100-tile grid
   re-buckets, colored by arm; anyone already in an arm stays put (sticky).
2. **Two users** — `alice` and `bob` show their arm side by side. Different
   subjects, different arms — the thing a boolean flag can't express.
3. **Convert** — click tiles to convert those subjects; the per-arm rate bars
   pull apart live.

## Test it

```bash
just e2e-abtest             # sticky, different-arms, 50/25/25 split, monotone, attribution, live SSE
```

The e2e defines a 50/25/25 experiment and proves assignment is sticky, two
subjects can land in different arms, the split holds across a cohort, raising a
weight is monotone (an arm only gains subjects), conversions attribute to the
right arm, and a conversion reaches a separate held-open SSE connection.
