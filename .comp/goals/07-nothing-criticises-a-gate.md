# Nothing criticises a gate — 🔴 human-led

**Traces to:** `docs/CURRENT.md` — *"a check that already passes on the base tree
accepts anything, and nothing looks"* — and the first real decomposed run on this
repository, which scored **1000** on two candidates that had deleted their own
component exports.

## What happened

A goal's checks are hand-authored, per goal, in TOML. That is the softest surface
in the system and the one an optimiser attacks first. Two ways it failed on the
first attempt, both inside an hour:

1. **The checks passed on the base tree.** `cargo component check -p demo` and
   `cargo component build -p demo` both succeed on a crate that implements none of
   its world — `pub fn nothing() {}` builds a "component" happily. So the stub
   passed, the models deleted `mod bindings` and `bindings::export!`, wrote plain
   Rust functions, and the gate reported a perfect score.
2. **The check's COMMAND reached the model.** A repair prompt carries the failing
   check, so a gate that greps for the field being negotiated hands the answer
   over: the frontend passed in round one without asking anybody anything.

## What is wanted

A **gate critic**: before a run spends anything, run the goal's checks against the
UNMODIFIED base tree and refuse the goal if they pass. Twenty lines, and it would
have caught both failures above — the second because a check that names its answer
usually also passes on the base.

Worth having beside it:

- a warning when a check's command CONTAINS a string the goal text asks the model
  to produce;
- the same criticism for a part's checks, not only the goal's;
- and a note in the run's summary saying which checks were criticised, because a
  silent guard is a guard nobody trusts.

## Why it is human-led

It changes what a run REFUSES to do, and a refusal that is wrong is worse than a
missing check: a goal whose checks legitimately pass on the base — a regression
test, a benchmark that must not get slower — is a real shape this would block. The
rule needs a way to say "yes, deliberately".
