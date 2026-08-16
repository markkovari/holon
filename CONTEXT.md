# Context

The domain language of Holon. A glossary, not a spec — no implementation
details, no decisions. Decisions live in [`docs/adr/`](docs/adr/).

## Artifact

A built WASM component, identified by its content digest, carrying the WIT
interfaces it imports and exports. A WAC composition is an artifact too. The
same component built twice is two artifacts.

## Interface

A named contract an [artifact](#artifact) imports or exports. Interfaces are how
artifacts are matched to each other, and they are what a [lesson](#lesson) is
about — a name two unrelated goals can share, unlike a goal's wording.

## Capability

What the system can *execute*. Held by artifacts, named by the
[interfaces](#interface) they export. Distinct from [knowledge](#knowledge): a
capability runs, a lesson only informs.

## Knowledge

What a [run](#run) *learned* — accumulated history, and unrecomputable. Distinct
from [capability](#capability), which is derived from artifacts and can always be
recomputed.

## Event

Something that happened, appended by whoever observed it. Any branch may append
one; appending is not a claim about what is true.

## Gate

What decides whether an [attempt](#attempt)'s work is acceptable. Its **verdict**
is both a score and the reasons behind it; the reasons are what the next attempt
reads, so a verdict is addressed to a future attempt, not to a person.

## Lesson

An [event](#event) the [gate](#gate) has blessed, and therefore the only kind
another [branch](#branch) will read. Every lesson is about an
[interface](#interface), not about a goal's wording.

## Pool

A shared collection that outlives the run that wrote to it. Reuse comes out of
a pool; what a run creates goes back into it.

## Shared

Within one project: across the branches of a single goal, and across goals over
time. Not across projects, machines, or people.

## Goal

A unit of work a person authors. Its execution — attempts, branches, verdicts —
is not the goal itself.

## Run

One execution of a [goal](#goal). Its **subgraph** is everything that execution
touched: [attempts](#attempt), [branches](#branch), artifacts built, lessons
written. A run is **resolved** when its goal is finished. Only a run is resolved —
a [composition](#composition) never is.

## Branch

One line of attack on a [goal](#goal), pursued independently of the others in its
[round](#round). Branches are kept whether or not they win: the losers are the
only evidence for why the winner won.

## Round

One generation of [branches](#branch), tried together. A round is the unit that is
abandoned as a whole — a second round exists because the first did not produce an
acceptable [attempt](#attempt), and it starts from what the [gate](#gate) said
about them.

## Attempt

One [branch](#branch)'s work in one [round](#round), and the thing a
[gate](#gate) judges. Two attempts in the same round happened *at the same time*;
two in consecutive rounds happened *because* the earlier one did not pass.

## Composition

An [artifact](#artifact) built by wiring other artifacts together. A composition
and its parts live in the [pool](#pool) independently: the parts stay reusable on
their own, and the composition is reusable as one thing.

## Outcome

The observed history of an [artifact](#artifact) — whether gates passed, how
often it was reused. Counted from events, never asserted.
