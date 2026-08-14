# Drive the queue — 🔴 human-led

**Traces to:** `docs/CURRENT.md` — *"Nothing picks a started goal off the queue.
`comp goal start` records that one started, and a person is still the wire
between that and a search."*

## What is wanted

A process — a mode of the reconciler, or a small sibling binary — that watches
the goal queue, and for each goal a human has moved to `running`, runs a
`generation::search` against the project's repo and lands the winner. The two
ends already exist and are tested; this is the wire between them.

## Why it is human-led, and why it stays careful

The whole reason a person still starts each goal is that the interruption rate —
how often the loop needs a human — is unmeasured, and every argument about
interfaces is really an argument about that number (ADR-0082). So this goal is
not "make it autonomous." It is: **pick up a started goal, run it once, and
report** — with a hard stop between runs, so the number can be watched before the
leash is lengthened.

It spans the reconciler's control loop, the platform's goal state machine
(`goal-may` transitions), and the forge, and it makes real decisions about
concurrency (one active run per project — the entire answer to concurrent pull
requests, ADR-0082) and failure (a failed goal is terminal and dead-letters, so a
retry is a new goal). That is why the agent is the wrong tool and a person leads
it.

## Surface (for a human)

- `reconciler/src/main.rs` (a `--goals` mode), or `reconciler/src/bin/goald.rs`
- `components/platform-domain/src/lib.rs` (the goal transitions)
- reuses `reconciler/src/generation.rs` wholesale
