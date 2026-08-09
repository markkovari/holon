# e2e — six authored manifests, one fleet, real requests

```
cargo build --release --manifest-path host/Cargo.toml
cargo nextest run --release --manifest-path reconciler/Cargo.toml -E 'test(six_manifests)'
```

Needs `nats-server` on PATH, the built `comp-host`, and the component artifacts
(`just build`, `just compose-gate`). Runs in about eight seconds.

The fixtures here are **authored** documents — what a person writes — and the test
converts them through `spec::AppSpec::to_manifest`, the same code path a real deploy
uses. Nothing in this directory contains a digest, a `host_needs` list nobody chose,
or an `egress` policy; those are stamped by the platform.

| fixture | must |
|---|---|
| `fused.yaml` | serve — a composed artifact over HTTP |
| `linked.yaml` | serve — `gate`'s two imports bound to `record-store` and `shaper` at runtime |
| `zero.yaml` | serve — `min: 0`, activated by the request itself (ADR-0042) |
| `conflict.yaml` | be refused — two providers of one interface |
| `ungrantable.yaml` | be refused — a capability no host grants |
| `unplaceable.yaml` | be refused — a constraint no node advertises |

## Why it is shaped like this

**Positives are checked by invoking them.** An app that is placed but does not answer
is exactly the failure a status check misses — inventory would show it running.

**Negatives are checked by their reason, not just by failing.** Asserting "it was
refused" passes for a refusal with the wrong reason, and a reason nobody can act on is
barely better than a crash. `conflict` must name the interface *and both providers*;
`unplaceable` must name the constraint it could not meet.

**They deploy together on purpose.** Three broken manifests alongside three healthy
ones is the assertion that a bad manifest cannot stop good apps from being placed — a
property no single-app test can show, and the kind of thing that breaks when the
planner grows a new early return.

**It polls rather than sleeping on a number.** Inventory is a heartbeat behind reality
and a parked app has to be activated first; a test that asserts on a snapshot taken at
the wrong moment fails on a working system. That mistake has been made twice in this
repo (ADR-0042, ADR-0045) and costs more than the polling does.

**One language, one runner.** The control plane is an axum stub inside the test rather
than a script, so the fixtures, the conversion, the assertions and the harness are all
Rust under `cargo nextest`. The previous bash+python version took about a minute; this
takes eight seconds, because the stub is in-process and nothing sleeps on a guess.

## Adding a fixture

Write the YAML, then add the app to the serve list or the refusal table in
`reconciler/tests/e2e.rs`. Refusal entries carry the substrings the reason must
contain — write the ones an operator would need in order to fix it, not the ones that
happen to be in the string today.
