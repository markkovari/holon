# The base moved under us — 🟡 needs a gate

**Traces to:** `docs/SCENARIOS.md` — *"the base moved while the branch worked …
nothing detects that the base moved before the PR opens. Rebase or refuse is an
open question (ADR-0082)."*

## What is wanted

`git:forge/base-commit` pins where a run started. Nothing checks, before opening
the pull request, that the base still points there. If it moved — someone landed
a commit while the swarm was thinking — the winner was judged against a tree that
no longer exists, and the pull request is either a silent conflict or a quiet
revert of that other commit.

The smallest honest fix: **refuse, loudly.** Before `land`, re-read
`base-commit`; if it differs from the run's pinned base, do not open the pull
request — return a distinct `base-moved` outcome naming both commits, so a person
(or a later goal) rebases and re-runs. Refusing is correct and cheap; rebasing
automatically is the harder second step and a separate goal.

Note that serialising goals — one active run per project — does **not** avoid
this: a human, or another project, or a direct push can move the base between a
run starting and its pull request opening. The check is needed regardless of the
queue.

## Surface

- **writable:** `components/graph-selector/src/lib.rs` (the `land` path) and its
  contract `components/graph-select/wit/select.wit` (the new outcome)
- **gate:** extend `reconciler/tests/select.rs` — a stand-in forge whose
  `base-commit` returns a *different* sha than the run pinned, asserting `land`
  refuses and names both. Write that failing test first (the 🟡).
