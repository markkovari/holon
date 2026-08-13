# Scenarios — how a graph run succeeds, and the ways it does not

Three levels of difficulty, each with what success looks like, what failure looks
like, and — the part that matters — **which of them can be tried today**.

The distinction this document keeps is between:

| | |
|---|---|
| ✅ **covered** | there is a test, and it has been seen to fail against the bug it exists for |
| ⚠️ **runnable** | the pieces exist and nobody has put them together |
| ❌ **blocked** | something named below does not exist yet |

The single largest ❌ is the **agent**: nothing turns a goal into a candidate. Every
scenario below that needs one is marked, rather than being written as though the
loop were closed. `comp-checks` and `graph:fitness` can judge anything handed to
them; nothing hands them anything yet.

---

## Level 1 — simple: one goal, one branch, one gate

A person writes `.comp/goals/cache.md`, starts the goal, one branch tries it,
the checks run, a pull request opens.

    goal ──▶ branch ──▶ candidate ──▶ checks ──▶ PR ──▶ human merges

### Success

| step | how it is known to work |
|---|---|
| the goal is queued and only a human starts it | ✅ `projects.rs` — waits six seconds and asserts it is still queued |
| the branch gets its own store | ✅ `environments.rs` |
| the candidate is judged by real commands | ✅ `fitness.rs` — 1000 for a candidate that fixes it |
| the winner becomes a commit and a PR | ✅ `forge.rs` — six calls, branch created last |
| a recompiled artifact actually replaces the running one | ✅ `newversion.rs` |

### Failure modes

| what goes wrong | what happens now |
|---|---|
| the agent produces no diff | ✅ refused — `git:forge` rejects a proposal with no changes, because a diff-less PR is what "reported success having done nothing" looks like |
| the gate is empty | ✅ refused — an empty check list would be *vacuously* accepted, which is how a swarm accepts everything |
| a check hangs forever | ✅ killed after the timeout, reported as killed rather than failed |
| a check names `rm -rf` | ✅ refused by the allow-list, as a failed check naming why |
| the candidate writes `../../etc/passwd` | ✅ refused by both the runner and the forge |
| the base moved while the branch worked | ❌ **unhandled.** `git:forge/base-commit` pins the start; nothing detects that the base moved before the PR opens. Rebase or refuse is an open question (ADR-0082) |
| the model is down | ⚠️ `provider-unavailable` surfaces; nothing retries or degrades to another tier |

**The honest summary of level 1:** every piece is covered except the agent in the
middle, and the base-drift question.

---

## Level 2 — complex: one goal, many branches, selection

A generation of eight branches explores the same goal; each is judged; the best
is promoted; the rest are closed.

    goal ──┬─▶ branch 1 ──▶ 1000  ← wins
           ├─▶ branch 2 ──▶  500
           ├─▶ …
           └─▶ branch 8 ──▶    0

### Success

| step | how it is known to work |
|---|---|
| eight branches spawn concurrently | ✅ `stress_env.rs` — 8/8 accepted, converged in 3.0s |
| each has its own store, none shares | ✅ asserted by name; the bucket-name collision at depth six is fixed and unit-tested |
| candidates are ranked when none is acceptable | ✅ `fitness.rs` — 1000 / 500 / 333, with the last two both failing the gate |
| derived work is computed once for the generation | ✅ `artifacts.rs` — twelve concurrent lookups, exactly one producer |
| closing a branch closes what grew from it | ✅ `stress_env.rs` |

### Failure modes

| what goes wrong | what happens now |
|---|---|
| every branch fails the gate | ✅ **the score still orders them** — this is the whole reason the runner reports a vector rather than a verdict |
| two branches produce identical candidates | ⚠️ `artifact:cache` would dedupe the derived work, but nothing dedupes the *candidates*, so both get gated and both get scored |
| all eight converge on the same idea | ❌ **herding, and it does not announce itself.** ADR-0081 names the mitigations — asymmetric visibility, a diversity budget, one branch that reads nothing — and none is built. The run looks healthy and the parallelism is worthless |
| scores tie | ❌ nothing breaks the tie; no rule exists |
| the fleet cannot place eight more branches | ✅ refused with a 429 naming the lag and the limit, rather than accepted and never started |
| a burst outruns the limit | ✅ counted against the last report, so 625 spawns are cut to 435 |
| the generation costs more than the budget | ❌ **nothing spends against the budget.** It is a field on a project that nothing enforces, which by this repo's own rule is documentation |

**The honest summary of level 2:** the mechanics work and the *strategy* does not
exist. Nothing decides how many branches, which to extend, or when to stop.

---

## Level 3 — very complex: a search, over time, with things breaking

Generations of generations, over hours, with hardware failing underneath and a
human in the loop.

    gen 1 ──▶ pick 2 of 8 ──▶ gen 2 ──▶ pick 1 of 4 ──▶ gen 3 ──▶ human ──▶ PR
                   │                         │
              6 closed                  3 closed

### Success

| step | how it is known to work |
|---|---|
| branches of branches | ✅ depth 4 measured, 5 generations side by side, ~3s a level |
| 341 apps across four generations | ✅ `stress_tree.rs` |
| two of three nodes SIGKILLed mid-run | ✅ recovered in 18s and 24s; nothing told the lattice |
| closing one first-level branch closes 85 descendants | ✅ measured |
| a human starts, the loop runs, a human lands | ⚠️ both ends exist (`comp goal start`, a PR); nothing joins them |

### Failure modes

| what goes wrong | what happens now |
|---|---|
| a node dies holding half the tree | ✅ inventory expires, the reconciler sees a gap, work is re-placed |
| desired state is larger than the platform will report | ✅ **fixed, and it was silent**: a fleet asked for 3906 apps sat at exactly 500 forever, every one past the cap accepted and never placed |
| the search never converges | ❌ no plateau detection, no `loop-until-dry`, no stopping rule at all |
| a branch runs out of fuel | ❌ **no fuel exists.** ADR-0081 designs conservation, escrow and refund-on-death; none is built. `quota:meter` is a rate limit and not a budget |
| a branch waits on a human for two days | ❌ suspension is designed (`awaiting-human`, environment released, fuel in escrow) and unbuilt. Today a branch would simply sit there holding a node |
| the knowledge pool fills with a wrong lesson | ❌ the graph stores and traverses; nothing promotes, weights by outcome, or decays |
| two runs race the same repository | ✅ **cannot happen by construction** — one active run per project, which is the entire answer to concurrent pull requests until somebody raises the limit |
| a run's base goes stale mid-search | ❌ unhandled, and it bites *with* a serial queue — serialising does not avoid it |
| the loop asks a human fifty times | ❌ **the interruption rate is unmeasured**, and every argument about interfaces is really an argument about that number |

**The honest summary of level 3:** the substrate survives things breaking. The
search does not exist — no fuel, no stopping rule, no selection strategy, no
memory that improves.

---

## What the matrix says, taken together

Read down the ❌ column and the shape is consistent. **Everything that carries
work is built and has been broken on purpose to prove it. Everything that
DECIDES is designed and unbuilt.**

    carrying    environments, vgit, forge, artifact cache, checks, admission
                → survived 341 apps and two dead machines

    deciding    the agent, fuel, selection, stopping, knowledge promotion
                → ADR-0081, marked proposed, none of it running

That is a deliberate order rather than an accident: a wrong decision on a
substrate that loses work is impossible to debug, and every mechanism above was
built by breaking it first. But it means the honest answer to "can the graph
succeed" today is:

> **A single goal can go from a queue to a pull request, and every step of that
> path is tested. Nothing yet chooses what to try, how much to spend, or when to
> stop — so a run does not fail, it simply never starts.**

The smallest thing that would change that is the agent: one component that turns
a goal and a tree into a candidate. Everything either side of it is built.
