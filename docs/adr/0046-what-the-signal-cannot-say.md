# 0046 — What the signal cannot say

Status: accepted. Closes the three gaps ADR-0045 recorded rather than leaving them as
comments.

Each is the same shape of bug: **two different situations producing identical
observations**, with only one of them a fault.

## 1. Saturated and wedged looked the same

A component whose replicas are wedged — holding connections they never complete —
pins in-flight at the bound and sheds everything behind it. By shed count alone that
is indistinguishable from honest saturation, and ADR-0045's rule would scale it up:
**manufacturing more wedged instances and making the outage bigger**.

The missing fact is whether the fleet is *answering*. The ingress now counts responses
a backend actually produced, per host per interval, and publishes it beside in-flight
and shed:

```rust
let demand = if served == 0 && shed > 0 { inflight } else { inflight + shed };
```

Any completed response counts, not just 2xx — a 429 from a rate limiter is the
component doing its job. The question is "is it answering at all", not "is it answering
happily".

A missing `served` field reads as "assume it is serving", so an older ingress
mid-rollout keeps ADR-0045's behaviour instead of having its refusals silently
discarded as wedged (ADR-0044).

## 2. At the ceiling looked like correctly sized

`max` is the operator's limit, so nothing in the platform can fix being pinned against
it — which is exactly why it has to be *said*. Before this, a fleet at `max` and
shedding produced the same output as a fleet that was comfortably sized: nothing.

`plan()` now returns `at_ceiling` alongside `unschedulable`, carrying what demand
actually asked for before it was clamped, and the reconciler logs it and posts it to
the control plane on the existing status endpoint:

```
eve/shop/gate is at its ceiling of 4 replicas and demand asked for 6
  — raise `scale.max` or accept the shedding
```

Observed firing 15 times during the benchmark's overload window.

## 3. No signal looked like no traffic

The one that actually bit. The reconciler opened the load bucket with `.ok()`, so a
failure produced `None`, which fell to `Default::default()` — an empty `Load` — which
every autoscaled app reads as "nobody is asking for me".

Logging it, as ADR-0045 did, is a band-aid: the *behaviour* was still wrong. An app
scaled to 6 would collapse to the manifest's `replicas` at the moment nobody could see
what it was carrying.

The fix is in the type. `plan` takes `Option<&Load>`, and absence is handled the way
ADR-0022 handles a failed poll — **we know nothing, so we change nothing**:

| | before | after |
|---|---|---|
| signal unreadable | shrink to manifest `replicas` | **hold what is running** |
| host has no sample yet | shrink to manifest `replicas` | **hold what is running** |
| genuinely idle app | scale toward `min` | scale toward `min` |

Holding is clamped to `[min, max]`, so a held count can never drift outside what the
operator allowed.

## Measured

Same bench as ADR-0045 — four nodes, `min 1 / max 4 / target 20`, `--max-inflight 8`:

```
replicas: 1 -> 4 -> 1
served:   188 748
ceiling notices: 15
```

## What is still not addressed

- **`served == 0` is a blunt test.** A component answering one request per interval
  while shedding thousands is nearly wedged, and this counts it as healthy. A ratio
  would be better and needs a threshold nobody has calibrated.
  `// ponytail:` zero is the only value that needs no justification.
- **Nothing distinguishes "wedged" from "cold".** An app that has just been activated
  has served nothing yet; for one interval it looks wedged and its refusals are
  ignored. Self-correcting on the next sample, and worth knowing.
- **`at_ceiling` is reported, not acted on.** No alert, no automatic raise. That is
  deliberate — `max` exists because someone chose it — but "reported into a log" is
  weaker than it sounds until something reads the log.
