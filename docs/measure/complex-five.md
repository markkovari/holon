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

### App 1, run 2 — three winners, no PR, and the real bottleneck

**Outcome: no PR. All three parts accepted at score 1000; the join rejected them.**

```
intake  accepted=true  score=1000  generations=1
assist  accepted=true  score=1000  generations=1
ledger  accepted=true  score=1000  generations=1
No PR opened: the halves pass alone and not together (score 0)
  the-whole-triage-assist-api-works:
    assist (src/assist.rs) wrote subject=['sess_2718e26dbce03e629e5c67a968bd6039']
```

One part wrote the **bearer token** into the audit trail where `principal.subject`
belongs — the wrong value, and a credential in a durable log. It passed its own gate,
because with `ledger` stubbed there is no trail for a per-part gate to read. Three
parts each correct alone, wrong together: the failure only a chain can produce, caught
by the only check that sees all three.

| measurement | value |
|---|---|
| parts accepted | 3/3 at 1000 |
| attempts recorded | 18 (3 parts × 6 branches) |
| branch pass rate, **first try** | 13/18 = **72%** — inside the 25–75% band |
| branch pass rate, final | 18/18 = **100%** — above it |
| attempts needing a repair try | 5 |
| median attempt, end to end | **311 s** |
| median by part | intake 90 s · assist 354 s · ledger 375 s |
| total attempt time | 85 min |
| shim timeouts | 0 |
| lessons written | **0** |

**Where the time goes.** The median attempt is 311 s and the spread is by part, not by
branch: `intake` at 90 s has a model-free gate, `assist` at 354 s makes two real model
calls in its gate, `ledger` at 375 s makes none and is still slow — so the cost is the
WRITER call, not the gate. Gate model calls measured 9–13 s each. The shim's documented
130 s median is a fact about long code-writing prompts, and it is the whole wall-clock
budget: nothing else in the loop is within an order of magnitude of it.

**Three findings, and the third is the answer to "where does it bind".**

1. **My per-part gates were too easy and the join carried all the discrimination.** A
   100% final pass rate is the over-specification signal this document pre-registered,
   and here it has a specific cause: an invariant that spans parts cannot be checked
   inside one. That is not a goal-writing mistake to be fixed by adding hints; it is a
   property of decomposing work into independently-judged parts. The band still did its
   job — it flagged the gates, not the agents.

2. **The lesson pool is populated backwards.** Run 1 — a broken sandbox — wrote three
   lessons, all `scored 0; … does not compile`, with `uses = 24` before anything read
   them usefully. Run 2 — three winners at 1000 — wrote **none**, because promotion
   only runs when a goal fully resolves, and the join failure ended the run first. So
   the graph currently learns from harness bugs and forgets successful work. Nothing in
   the store distinguishes "the candidate was wrong" from "the tree it was handed could
   not build".

3. **A join failure is terminal, and its verdict is addressed to nobody.** `CONTEXT.md`
   says a verdict is "addressed to a future attempt, not to a person". The join's
   verdict is addressed to neither: no part owns it, no repair round is spawned (rounds
   remained unused — the parts were already accepted), no lesson records it, and the run
   ends. Everything needed to fix it was known — the offending value, the event, the
   part — and none of it reached anything that could act. **This is the bottleneck for
   holon working properly on complex apps:** not the model, not the pool, not the
   graph, but that cross-part failure is discovered exactly once, at the last possible
   moment, and then discarded.

Two smaller ones worth recording: `goalrun` refuses to run when the registered
contract differs from `CONTRACT.md` (correct — an amendment belongs in ask/answer), but
`SURREAL_DB` is hardcoded to `goalmemory`, so a contract fix found BY a failed run makes
that run's database unusable and there is no per-experiment namespace. And the gate
verdict that caught this originally named no part at all; it now names the file, which
is the difference between a finding and a repair instruction.

**Taxonomy for this run:** 5 `behaviour` (first-try failures, all repaired), 1
`contract-drift` (the join). Zero `compose-or-host`, zero `shim-timeout`, zero
`wrong-or-missing-capability` — **every branch used the pooled capabilities rather than
reimplementing them**, which is the first real evidence in this experiment that the
import assertions do their job and that the pool is being reached for.

### App 1, run 3 — a pull request, and what the graph actually did

**Outcome: [PR #90](https://github.com/markkovari/holon/pull/90). Composition passed at
score 1000.** 385 lines across three files, each part calling the capabilities its
world hands it: `intake` → authorizer + limiter + redactor + store, `assist` →
authorizer + ai:inference + store, `ledger` → authorizer + audit:log. No
reimplementation of any pooled capability in any accepted part.

| measurement | run 2 | run 3 |
|---|---|---|
| parts accepted | 3/3 | 3/3 |
| branch pass rate, first try | 13/18 = 72% | 15/18 = **83%** |
| median attempt | 311 s | 322 s |
| median by part | 90 / 354 / 375 s | 90 / 307 / 391 s |
| total attempt time | 85 min | 82 min |
| capability-import failures | 0 | 0 |
| join | rejected | **passed** |
| lessons promoted | 0 | **0** |

83% first-try is **above** the pre-registered 25–75% band, and the cause is known and
self-inflicted: run 2 discovered a cross-part invariant and I wrote it into the
contract before run 3. That is the over-specification signal working as designed —
recorded as such, not as a win. The honest reading of app 1 is: 72% on the version of
the goal that had not yet been sharpened, 83% after one hint.

#### What the knowledge graph does, and does not, do for a multi-part goal

This is the question the experiment exists to answer, and the answer is specific.

`goalrun.rs:1392` returns into `decomposed()` **before** the lesson and capability
machinery of the ordinary path is reached. What each path gets:

| mechanism | single-part goal | multi-part goal (every complex app) |
|---|---|---|
| recall lessons per branch (`Memory::recall`) | yes | **yes** — `compose.rs:546`, per PART, on that part's own goal, with a control arm that reads nothing |
| write failure lessons (`observe_failure`) | yes | **yes** — `compose.rs:515` |
| attribute a reading to an outcome (`attribute`) | yes | **yes** — `compose.rs:526` |
| **promote a winner into `patterns`** (`Memory::promote`) | yes — `goalrun.rs:1561` | **NO — nothing calls it** |
| capability search over the built pool (`search_the_pool`) | yes | **NO — after the early return** |
| pool context injected into the prompt (`pool_context`) | yes | **NO — after the early return** |
| operator visibility (`branch-i reads N lesson(s)`, distinct-reading tally) | printed | **not printed** |

Measured consequence, after three runs of app 1: the pool contains **only `errors`
rows**. Run 3's, in full — `ns: errors, goal: ledger, score: -1, promoted: false,
uses: 0`. A run that produced three winners and a merge-ready pull request promoted
nothing, because in the decomposed path there is no code that could.

So for the class of app this experiment is about:

* **Capability reuse is real and total — and it does not come from the graph.** Zero
  import-assertion failures across 36 attempts in runs 2 and 3. What drove it was the
  WIT world plus a contract saying "you call them; you do not write them", enforced by
  a gate that reads the artifact's imports. The capability search never ran and the
  pool context was never in a prompt. The component pool is being exploited through
  the *interface*, which is the cheaper and more reliable of the two paths.
* **The knowledge graph contributes failure memory and nothing else.** It can tell a
  later branch what went wrong; it is structurally incapable of telling one what
  worked, because `promote` is unreachable from the only code path a complex app takes.
* **And it is invisible.** Neither run printed a single line about what any branch read,
  so "did the lessons help" cannot be answered from a run's own output — only by
  querying SurrealDB directly, as this document had to.

That is the effectiveness answer, and it is not "the graph is useless". It is that on
multi-part goals the graph is running at roughly half its design: the read path works,
the write path only records failures, and the promotion path — the one that turns a
win into something reusable — is not wired in.
