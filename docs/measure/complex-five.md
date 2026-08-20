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

### App 1, run 1 — a harness failure, and the most useful one available

**Outcome: no PR. 0/3 parts accepted, 0 branches passed, 36 model calls spent, and not
one of them was about the goal.**

Every branch was told the same thing:

```
error: failed to create a target world for package `triage-assist-domain`
Caused by: No such file or directory (os error 2)
```

`components/triage-assist-domain/Cargo.toml` names `../llm-inference/wit` among its
WIT target dependencies — `ai-inference` is orchestration over `llm:inference`, so the
manifest must see that package. `components/llm-inference/` was not in the goal's
`base_paths`, so it did not exist in the sandbox, so `cargo component` could not build
a target world, so nothing compiled. In the repository every gate passed; in the tree
the loop actually hands a branch, every gate failed identically.

What that one missing line cost, and what each piece of it teaches:

| observation | what it means |
|---|---|
| all 4 checks failed on the base tree, and the loop reported *"every check fails on the base tree, so every check can judge"* | the precheck only asks **whether** a check fails, never **why**. A tree that cannot build is indistinguishable here from work not yet done — and it reads as healthy |
| branches spent generation 1 filing `CONTRACT-REQUEST`s titled *"Cargo.toml is missing from the repo"*, *"Build failure appears environmental, not a contract gap"* | the agents diagnosed it **correctly**. The negotiation channel worked; there was simply nothing a part could do, because no part may write a manifest |
| several answers contained no file blocks at all — *"the logic is already correct"*, *"I traced this carefully rather than guessing a code fix"* | shown an unimplemented file and an impossible build, a branch reviews instead of writing. Not laziness: there is no edit that fixes a missing directory |
| repeated *"the same candidate an earlier attempt already produced"* | with no new information between attempts, the search collapses to one point. Rounds cost real time and bought nothing |
| 1 branch died on `claude -p exceeded 540000ms` | the shim's timeout is real and does fire. 1 in 36 |
| 2 parts asked, unprompted, for `id:generate`'s exact signature | a genuine contract gap, on a capability nothing needed: `audit-log` mints event ids when `id` is empty and `records:store` mints report ids. Written down now, and the import removed |

**Taxonomy for this run:** 36/36 `compose-or-host` (a sandbox that could not build). Zero
`behaviour`. A run that says nothing about the agents, the pool, or the graph.

**The finding worth keeping** is not "add the directory". It is that a goal's
`base_paths` is a **dependency declaration with no checker**, whose failure mode is to
fail every branch at once with a message that points at the wrong culprit, while the
loop's own precheck calls the gate ready. Two things now exist because of it:

* `tools/goal-rehearse.sh` — reconstructs the sandbox from `base_paths` +
  `keep_members` and runs the goal's checks under the same cleared environment
  `comp-checks` uses, in both directions: every check must FAIL on the base tree for a
  reason that is not a build error, and every check must PASS with a reference
  implementation applied. App 1 now does both. This is a required step before any of
  the remaining four runs.
* `compose::criticise` carries the base-tree failure **reasons** out, and `goalrun`
  prints them. It does not refuse on a heuristic — "compile" appearing in a gate's
  own prose is not proof of anything — but an operator can now see that all four
  checks failed for the same non-judgeable reason, in the second before the money is
  spent.

Cost of the lesson: one run. Cost had app 2 through 5 been authored first, as the
original plan had it: five.

<!-- One table per app, appended as each run completes. Do not edit anything above
     this line once the first run has started. -->
