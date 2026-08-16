# Context

The domain language of Holon. A glossary, not a spec — no implementation
details, no decisions. Decisions live in [`docs/adr/`](docs/adr/).

## Artifact

A built WASM component, identified by its content digest, carrying the WIT
interfaces it imports and exports. A WAC composition is an artifact too. The
same component built twice is two artifacts.

## Capability

What the system can *execute*. Held by artifacts, named by the interfaces they
export. Distinct from [knowledge](#knowledge): a capability runs, a lesson only
informs.

## Knowledge

What a [run](#run) *learned* — accumulated history, and unrecomputable. Distinct
from [capability](#capability), which is derived from artifacts and can always be
recomputed.

## Event

Something that happened, appended by whoever observed it. Any branch may append
one; appending is not a claim about what is true.

## Lesson

An [event](#event) the gate has blessed, and therefore the only kind another
branch will read. Every lesson is about an [interface](#capability), not about a
goal's wording.

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
touched: attempts, branches, artifacts built, lessons written. A run is
**resolved** when its goal is finished. Only a run is resolved — a
[composition](#composition) never is.

## Composition

An [artifact](#artifact) built by wiring other artifacts together. A composition
and its parts live in the [pool](#pool) independently: the parts stay reusable on
their own, and the composition is reusable as one thing.

## Outcome

The observed history of an [artifact](#artifact) — whether gates passed, how
often it was reused. Counted from events, never asserted.
