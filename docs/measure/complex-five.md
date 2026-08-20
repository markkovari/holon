# Five complex apps, and where the loop actually binds

An experiment, not a feature. Five apps are authored as goals and run through
`comp-goalrun` to a pull request, to answer one question the repository cannot answer
by inspection:

> Does a pool of built capabilities and a graph of past lessons let the loop assemble
> a whole it has never seen — and what stops it when it does not?

Everything below the line marked **pre-registered** was written before the first run.
That is the whole point of writing it early: an order chosen after seeing results, or
a band widened to fit them, measures the author rather than the loop.

## Pre-registered

### The five, and the order they run in

Simplest first, so a harness bug is found on the cheapest app. Every capability each
app needs already exists in this repository; none of the compositions does.

| # | app | capabilities composed | the hard interaction |
|---|---|---|---|
| 1 | `triage-assist` | auth-guard, rate-limiter, pii-redact, ai-inference, audit-log, record-store, id-generate | redaction must happen **before** the model call |
| 2 | `doc-search-agent` | mfa-authgate, quota, search-index, ai-inference, cache | retrieval, and refusing when the budget is spent |
| 3 | `moderation-queue` | session-store, throttle-domain, ai-inference, policy-guard, event-bus | a deterministic policy overrides the model |
| 4 | `support-desk` | webauthn, quota, ai-inference, notify-dispatch, outbox | at-least-once delivery of what the model wrote |
| 5 | `invoice-copilot` | auth-guard, rate-limiter, ai-inference, money, ledger, idempotency-guard | the model's output touches money, so it cannot be trusted directly |

Five different hard interactions on purpose. Five skins on one interaction would find
one bottleneck and report it five times.

### Run configuration

| knob | value | why this and not the default |
|---|---|---|
| `--branches` | 6 | selection needs something to choose between, and 6 gives up to 12 branch-level samples per app with two rounds |
| `--rounds` | 2 | the only place the lessons path is exercised *inside* one app rather than only across apps |
| `--attempts` | 2 | the default; a repair reads the gate's reasons |
| `--skip-above` | 1.0 | never skip. At the default 0.9 a re-run after a HARNESS failure is skipped as already-done, which would read as a pass |
| model | `CLAUDE_MODEL=sonnet` on the shim | the shim ignores the request's model, so the writer model is the shim's env; haiku on goals this complex would depress the success rate being measured and the finding would be "the model" |
| provider | `tools/claude-shim.mjs`, concurrency 4 | the subscription, not the API. Queueing is what makes a throttle read as *waiting* rather than as a branch failing |
| PR | one per app, no `--dry-run` | the forge path is part of what is being tested |

### What counts as success

Two units, and the second is the one that can embarrass a goal:

* **run-level** — did the run produce a landable candidate and a pull request. Target
  **5/5**. This is what "works properly" means.
* **branch-level** — what fraction of branches passed the gate. Target is a **band,
  25–75%**. Below it the goal is under-specified or the harness is broken. *Above* it
  the goal leaked the answer: ADR-0082's triage goal had 12 branches pass first
  attempt at score 1000, selection had nothing to choose between, and the search had
  stopped being a search. A 100% branch pass rate is written up here as
  over-specification, not as a win.

### How the pool and the graph are measured

There is no ablation arm: lessons stay on and the pool stays whole for every run
(turning off what the loop is for would measure a different system). So the evidence
is observational, and these four are what the runs already produce:

1. **Capability search** — what `search_the_pool` offered each goal, and whether the
   winner used it.
2. **Import assertions** — every part's gate reads the compiled artifact's imports
   (`gate_requires_capability`). A branch that reimplemented a pooled capability and
   answered every request correctly still fails. Reuse is therefore a pass/fail fact
   per branch, and the count of branches that failed *only* that assertion is the
   closest thing here to a measurement of the pool's pull.
3. **Lessons** — per-branch reads, the distinct-reading tally, what was promoted, and
   how the built-in control branch (`goalrun.rs`, the branch that reads nothing)
   scored against its siblings.
4. **Order** — app 5 runs with four apps' lessons behind it and app 1 with none.

**The confound, stated in advance:** the ordering effect above is confounded with app
difficulty and with the author's scaffolding improving across the five. It is not a
learning curve and will not be reported as one. The one honest cross-app signal is
whether the control branch's deficit against its siblings *grows* from app 1 to app 5.

### Failure taxonomy

Every attempt that does not pass is classified into exactly one bucket, and the
ranking of these buckets across all five apps is the answer to "where is the
bottleneck":

`compile-error` · `wrong-or-missing-capability` (failed an import assertion) ·
`contract-drift` (parts disagreed) · `token-wall` (`max_tokens` exhausted) ·
`gate-flake` (the gate failed for its own reasons) · `shim-timeout` ·
`compose-or-host` · `behaviour` (compiled, composed, ran, answered wrongly).

`behaviour` is the only bucket that represents the loop working as intended and
losing on merit. A taxonomy dominated by anything else is a finding about the harness,
not about the agents.

## Results

Nothing yet. App 1's harness is validated (its three gates pass against a throwaway
reference implementation, each part in isolation with its siblings stubbed, and all
three fail on the stub tree with an actionable reason); the first run has not been
made.

<!-- One table per app, appended as each run completes. Do not edit anything above
     this line once the first run has started. -->
