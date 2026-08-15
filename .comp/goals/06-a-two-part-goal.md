# A two-part goal — 🟢 agent-ready

**Traces to:** ADR-0086 (*parts negotiate a contract*), and the machinery in
`compose::run_parts`, `contract:registry` and `generation::compose_search`.

## What it is

The smallest goal in this repo that is genuinely TWO halves: a component that
implements `demo:shape/pager`, and a probe that calls it. They share one interface
(`components/demo/wit/demo.wit`), each gates alone against it, and neither may
edit the other's files.

    contract = "components/demo/wit/demo.wit"

    [[part]] component  → components/demo/src/lib.rs        cargo component check -p demo
    [[part]] probe      → components/demo-probe/src/lib.rs  cargo component check -p demo-probe
    [[check]] the join  → both crates check together

## How to run it

```bash
just build && (cd host && cargo build --release) && (cd reconciler && cargo build --release)
docker compose -f infra/compose.yaml up -d surreal

reconciler/target/release/comp-goalrun --smoke \
  --checkout . --repo markkovari/holon --base main \
  --anthropic-key ~/.comp-secrets/anthropic --github-token ~/.comp-secrets/ghpat \
  --surreal-url http://127.0.0.1:8000 --surreal-password ~/.comp-secrets/surreal
```

Drop `--smoke` for the real run; `--branches 1 --rounds 2` keeps the first one
cheap. The goal spec itself goes in `.comp/goal.toml`, which is where
`comp-goalrun` looks.

## What it is for

Exercising the decomposed path on something real rather than a fixture: two parts,
per-part gates, a contract both build against, a composition gate neither half can
pass alone, and one pull request. The negotiation is OPTIONAL here — the interface
is complete enough that neither half should need to ask — which is the honest
starting point: prove the loop, then write a goal whose contract is deliberately
short of something.
