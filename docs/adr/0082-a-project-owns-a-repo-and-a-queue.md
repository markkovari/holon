# ADR-0082 — a project owns a repo and a queue

*Where the repo, the credentials and the work-to-be-done finally live.*

**Status:** accepted; the project model and the queue are built, the runner deliberately is not — see *What is deliberately not built here*.

## The gap

Everything the graph loop needs now exists — environments that nest, a git object
store, a forge that opens pull requests, a scripted provider, an artifact cache —
and every one of them is configured in a **fixture**. Which repository, which
token, which egress, which base branch, what it may spend: all of it is either
hard-coded in a YAML file written for a test, or nowhere at all.

A project is the owner those things have been missing. Not a new subsystem: a
home for facts that already exist and currently belong to nobody.

## The decisions

### One repository per project

Multi-repo is a real case — a service and its client, changed together — and it
is deliberately **not** built. One repo makes the base commit unambiguous, the
gate a single command, and a pull request a single thing. Two repos means
coordinated commits, two bases that can move independently, and a merge that has
to land atomically or not at all.

Recorded as an open goal rather than designed for. A `repos: []` list would be
the shape, and every downstream thing that says "the base" would have to become
"the base of each" — which is exactly the kind of change that is cheap to make
once and expensive to guess at in advance.

### A failed goal goes to a dead-letter queue

Not retried. An LLM run that failed and is re-run **unchanged** costs money and
usually fails the same way — the interesting failures are specification failures,
and those need a person to change something, not a machine to try again.

So a failure is terminal, carries its reason, and sits in a queue somebody looks
at. Requeueing is an explicit act that creates a *new* goal, which keeps the
history of what was tried honest.

### A human always starts a goal

The queue does not drain by itself. It is a **worklist somebody pulls from**, not
a pipeline that runs.

This is the largest simplification available, and it settles the question
ADR-0081 left open about interruption rate: there is at minimum one interaction
per goal, at the moment where stopping is free. It also means **no autonomous
runner loop is needed at all** for the first version — nothing has to hold a
lease, decide what to work on next, or be trusted to spend money while nobody is
looking.

Two human touchpoints per goal, which is exactly what ADR-0081 argued for:

    a person starts it  →  it explores on its own  →  a person lands it

### One active run per project

Which is the whole answer to concurrent pull requests: there are none. That
problem does not need solving until `max-concurrent-runs` goes above one, and
that is the moment to design for it rather than now.

## What it is built from

Almost nothing here is new:

| need | already built |
|---|---|
| the queue, and `where state = queued` | `records:store` secondary indexes |
| a lifecycle that refuses illegal jumps | `fsm:workflow` |
| tokens, by reference | `holon secret set`, `comp:secrets/reader` |
| the base sha, the branch, the commit, the PR | `git:forge` |
| the branches within a run | environments (ADR-0078, nesting since the stress tests) |
| repeat work made free | `artifact:cache` |

The last row is worth stating plainly, because it is the reason projects earn
their keep beyond tidiness: **every goal in a project works on the same
repository.** The chunk index, the embeddings, the base-tree analysis — computed
once, reused by every goal in that project forever. Without a project there is
nothing for that cache to be scoped to.

## The shape

A **project** owns a repo and the policy around it:

    name, repo (owner/name), base branch
    forge-token-ref, llm-key-ref        vault references, never values
    egress, model tier, budget per run
    max-concurrent-runs = 1

A **goal** is one item on the worklist:

    project, title, spec (a path in the repo)
    state, priority
    run, pull request, what it cost
    failure reason, when it failed

### The lifecycle

    queued ──start──▶ running ──▶ awaiting-human ──▶ done
      │                  │
      │                  └──fail──▶ failed  (the dead-letter queue)
      └──abandon──▶ abandoned

`start` is the only transition a person must make for work to happen. `fail` is
terminal and carries a reason. Nothing moves out of `failed` — a requeue creates
a new goal, so what was tried stays visible.

## A goal is frozen when it starts

Editing a *queued* goal is ordinary. Editing a *running* one forks it into a new
goal, because ADR-0081 requires the spec to be content-addressed at run start and
this is that rule arriving at the queue. A spec that changed underneath a run
makes every comparison in that run a lie.

## The thing this does not solve, and should be said out loud

**The base moves.** Goal two branches from the base goal one just merged, which
`git:forge/base-commit` handles at the start of a run. What is unhandled is a run
whose base goes stale *while it is working* — someone else merged, and the tree it
has been reasoning about is no longer the tree it would land on.

Rebase, or refuse and requeue? That bites with a strictly serial queue and one
human, so it is not a concurrency problem and serialising does not avoid it. It
is left open here because the answer depends on how long a run takes, which
nothing has measured yet.

## What is deliberately not built here

- **The runner.** A goal can be started and its run recorded; what a run *does*
  needs the agent and the gate, which do not exist. The queue is buildable and
  testable now, and pretending otherwise would produce a state machine with
  nothing behind it.
- **Multi-repo**, as above.
- **Auto-retry**, as above.
- **Budget enforcement.** The field exists on a project; nothing spends against
  it yet, and by this repo's own rule (ADR-0081) a limit nothing enforces is
  documentation. It is written down as a project field because the *shape* is
  known, and it is named as unenforced here so it cannot be mistaken for working.
