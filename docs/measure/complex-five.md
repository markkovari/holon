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

## App 2 — `docsearch:agent`

**Outcome: [PR #91](https://github.com/markkovari/holon/pull/91) on the first run.**
Composition passed at 1000; 366 lines across three files.

| measurement | app 1 (run 3) | app 2 |
|---|---|---|
| parts accepted | 3/3 | 3/3 |
| branch pass rate, first try | 15/18 = 83% | **17/18 = 94%** |
| median attempt | 322 s | 273 s |
| median by part | 90 / 307 / 391 s | library 58 / answer 273 / stepup 325 s |
| total attempt time | 82 min | 69 min |
| capability-import failures | 0 | 0 |
| join | passed | passed |
| lessons written | 1 (an error) | **0 — the `memory` table was never created** |

### The finding: writing a reference first makes the goal too easy

94% first-try is far outside the pre-registered 25–75% band, and the cause is now
visible as a *practice* rather than an accident. `tools/goal-rehearse.sh` requires a
reference implementation to prove the gates are passable — and writing one means hitting
every trap in the interfaces personally, which means writing every one of them into the
contract. App 2's contract went into its first run already naming the `Exceeded`-is-not-a-duration
trap, the JSON-encoded `find_by` value, the title-not-in-a-hit problem and the order of
the five checks.

This repository already knew the shape of that mistake. `.comp/goal.toml`'s own header
records it: *"The first version of this goal wrote down every trap the author hit while
building the reference. Twelve branches then passed on the first attempt at score 1000 —
selection had nothing to choose between … the search had stopped being a search."*

So the two practices are in direct tension, and both are correct:

* **rehearse with a reference**, or a harness bug fails every branch identically and
  costs a whole run (app 1, run 1: 36 model calls on a missing directory);
* **do not write the reference's lessons into the goal**, or every branch passes and
  selection has nothing to choose between (app 1 run 3 at 83%, app 2 at 94%).

The way to hold both, and what apps 3–5 will do: **write the reference, then delete from
the goal every hint that the reference taught, keeping only what fails SILENTLY.** That
is the line ADR-0082 already drew; the discipline it needs is to apply it *after* the
reference exists, not before, because until then an author cannot tell which traps are
silent and which are loud. A goal is over-specified by exactly the traps that would have
announced themselves.

### The graph, measured across four runs

App 2's database ends the run with **no `memory` table at all**. Tables present:
`attempt`, `event`, `run`. Nothing was ever written for it to create.

Across every run so far:

| run | outcome | lessons written |
|---|---|---|
| app 1, run 1 | broken sandbox, every branch failed | 3 (all `does not compile`) |
| app 1, run 2 | 3 winners, join rejected | 3 (errors) |
| app 1, run 3 | **PR opened** | 1 (an error, from the one branch that never recovered) |
| app 2 | **PR opened, first try** | **0** |

Reading `compose.rs:505`, the mechanism is exact: a failure lesson is written only for an
entry that is **not accepted in the last round of a part's outcome**. A branch that fails
once and is repaired teaches nothing, because by the end its entry is accepted. And
promotion — the only path that records what WORKED — is never called on the decomposed
path at all. Put together:

> On a multi-part goal, the knowledge graph accumulates nothing from success, and
> accumulates from failure only when a branch never recovers. The cleaner the run, the
> less the pool learns. App 2 is the limit case: a perfect run taught it nothing.

That is not a small gap and it is not "the graph is broken" either — the read path works,
the control arm exists, attribution works. What is missing is that on this path there is
no writer for the one kind of knowledge worth keeping.

## App 3 — `moderation:queue`

**Outcome: [PR #92](https://github.com/markkovari/holon/pull/92) on the first run.**
Composition passed at 1000; 379 lines across three files.

| measurement | app 1 (run 3) | app 2 | app 3 |
|---|---|---|---|
| branch pass rate, first try | 15/18 = 83% | 17/18 = 94% | **15/18 = 83%** |
| median attempt | 322 s | 273 s | 331 s |
| median by part | 90 / 307 / 391 s | 58 / 273 / 325 s | intake 47 / verdict 331 / queue 382 s |
| total attempt time | 82 min | 69 min | 75 min |
| capability-import failures | 0 | 0 | **0** |
| lessons written | 1 (an error) | 0 | 1 (an error) |

### Stripping the loud hints moved the rate, by about as much as it should have

App 3's goal is the first written under app 2's lesson: keep only what fails silently,
delete every hint whose absence produces a loud failure a branch can read for itself. Its
header records which hints were cut and why, before the run.

The first-try rate went 94% → 83%, back to the top of the pre-registered band rather than
far outside it. Three branches failed their first attempt and repaired; `verdict` — the
part carrying the precedence trap — was the hardest, with only 4 of 6 passing first try.
That is what a search with something to choose between looks like, and it cost nothing:
the same three parts, the same one round, a pull request either way.

The discipline is cheap and specific enough to state as a rule: **an author cannot tell a
silent trap from a loud one until they have written the reference, so the goal must be
edited down after the reference exists, never before.**

### Reuse, measured three ways

`tools/reuse-ratio.py` reports from three independent sources: the components `comp-plug`
wires in (derived from the compiled artifact's imports), non-comment Rust lines on each
side, and the interfaces the artifact IMPORTS against the ones its world offers. Generated
`bindings.rs` is excluded from both sides — including it would flatter reuse by tens of
thousands of lines and measure `wit-bindgen` rather than reuse. Measured on the landed
pull requests, not on the repository's stub tree:

| app | components wired | reused sloc | written sloc | ratio | capabilities offered → imported |
|---|---|---|---|---|---|
| `triage:assist` | 6 | 2901 | 309 | 90.4% | 11 → **11** |
| `docsearch:agent` | 7 | 3055 | 301 | 91.0% | 11 → **11** |
| `moderation:queue` | 6 | 2765 | 379 | 87.9% | 10 → **10** |
| **all three** | — | **8721** | **989** | **89.8%** | 32 → **32** |

Three things worth saying about that number.

**It is a floor.** `--wiring` lists a component's own providers, not their transitive ones
— `anthropic-provider` behind `ai-inference` is real code in the deployed graph and is not
counted here.

**The capability column is the stronger claim.** 32 capabilities offered across three
apps, 32 actually imported by the compiled artifacts: not one part reimplemented a pooled
capability in any accepted candidate. That is the property the gates assert directly, and
it held across 54 attempts.

**The scaffold is the honest asterisk.** Each app also carries a router — 268, 271 and 286
lines — that a person wrote, not the run. Counted as authored code the ratios become
85.0%, 84.2% and 80.6%; counted as what it is, harness rather than product, they are the
numbers above. Both readings are in the table's data; neither changes the capability
column.

## App 4 — `support:desk`, and six runs to get there

**Outcome: [PR #93](https://github.com/markkovari/holon/pull/93) on the sixth run.** Five
runs produced nothing, and not one of them failed because an agent could not write the code.

| run | outcome | why |
|---|---|---|
| 1 | no PR | my contract described a component that does not exist; a branch found it, the amendment was granted, and it un-accepted a part that had already passed at 1000 |
| 2 | no PR | a gate crashed with `JSONDecodeError` instead of failing; branches were handed a stack trace and answered, correctly, that nothing in their file was wrong |
| 3 | no PR | another unguarded parse, same shape, same result |
| 4 | no PR | all three parts accepted at 1000; the JOIN failed because my check compared a stored string against `json.dumps(arrived)`, and the em dash the model wrote became `\u2014` |
| 5 | no PR | the provider was down — `claude -p exited 1` on every branch, a hard account limit rather than contention |
| 6 | **PR #93** | 3/3 parts at 1000, join passed |

### The run that landed

| measurement | value |
|---|---|
| branch pass rate, first try | **12/18 = 67%** — the first app inside the pre-registered 25–75% band |
| by part, first try | tickets 6/6 · reply 6/6 · **courier 0/6** |
| median attempt | 279 s (tickets 64 · reply 279 · courier 412) |
| total attempt time | 78 min |
| capability-import failures | 0 |
| lessons written | 0 |

`courier` needed a repair attempt on **every single branch** and all six then passed. That is
the gate working as designed rather than the part being impossible: at-least-once delivery
has four separate ways to be wrong, the gate breaks a real webhook sink to find them, and no
branch got it right first time. It is also the clearest calibration signal in the five apps —
a part nobody passes cold and everybody passes on the second look.

### What the five failed runs are actually evidence of

Not fragility of the loop. Every one was a defect in the harness a person wrote, and the
agents were the ones who found two of them:

* a branch filed *"notify:dispatch's delivery-failed doc comment contradicts CONTRACT.md's
  courier spec"* — and was right. `send` returns `Ok(status)` only for a 2xx and
  `Err(DeliveryFailed)` otherwise, so the rule my contract was built on described nothing.
* a branch filed *"courier gate returns empty HTTP body (JSONDecodeError on the test side)"*
  — also right, and by then I had already patched two crash sites one at a time instead of
  auditing them.

**The measurement that matters here is the ratio: six runs, five harness faults, zero cases
of an agent unable to do the work.** On a goal whose contract and gates are correct, three
parts written by three agents against one contract passed on the first generation every time
it was tried — app 2, app 3, and app 4's run 6.

### What the failures changed

`tools/goal-rehearse.sh` gained the direction that would have caught the fictional rule:
**VIOLATORS**. Each is the reference with one stated rule broken and a `must-fail` file naming
the check that must reject it, and a violator that survives means the rule is either
untestable or untrue of the components. App 4 has five, and one survived on the first try —
the contract said "read what `outbox::fail` returned and report abandoned replies" while the
gate asserted the dead-letter list, which the outbox fills whatever the courier reads.

A **crash is now its own verdict** in all three directions: a check may fail, it may not
raise. That detector took two corrections, both instructive. `assert` is the intended failure
mechanism and python prints a traceback for it, so "contains a traceback" flagged every clean
rejection; and a guard that catches a parse error and re-raises `AssertionError` with
something readable prints BOTH exceptions chained, so only the one that TERMINATED the process
tells you whether the gate judged or broke.

Two more, from giving the rehearsal its own build directory: a rehearsal must never share
`cargo-target` with a live run (same crate name, so each tests whatever the other wrote last),
and four of the five apps' gate build lists omitted `audit-log` — an import of `auth-guard`'s
own world — passing only because a warm shared cache held it from another app.

## Reuse across four landed apps

| app | components wired | reused sloc | written sloc | ratio | capabilities offered → imported |
|---|---|---|---|---|---|
| `triage:assist` | 6 | 2901 | 309 | 90.4% | 11 → **11** |
| `docsearch:agent` | 7 | 3055 | 301 | 91.0% | 11 → **11** |
| `moderation:queue` | 6 | 2765 | 379 | 87.9% | 10 → **10** |
| `support:desk` | 7 | 2931 | 304 | 90.6% | 11 → **11** |
| **all four** | — | **11652** | **1293** | **90.0%** | 43 → **43** |

Forty-three capabilities offered across four worlds, forty-three actually imported by the
compiled artifacts. Across 72 attempts, no accepted part reimplemented a pooled capability.
The ratio is a floor — `--wiring` lists a component's own providers and not their transitive
ones — and the routers a person wrote (268–287 lines each) are the honest asterisk: counted as
authored code the ratios fall to 80–85%, and the capability column does not move either way.
