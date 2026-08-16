# ADR-0090 — a lesson is about a capability, not a sentence

*The repository has two graphs and no edge between them. One knows that
`clinic-domain` imports `csv:codec/codec` and that thirty-nine apps carry
`record-store`. The other knows that somebody once lost three rounds to
`Dialect.delimiter` being a `String`. Neither can reach the other, so the second
fact is retrieved by guessing at the wording of a goal.*

**Status: proposed. The join is the missing edge, not a missing store.**

## The two graphs

**The capability graph** (`comp-capgraph`, ADR-0087, ADR-0089) is *derived*. It is
recomputed from the built artifacts in under a second and is therefore always
true: 150 components, 80 consumed interfaces, 300 import edges, 56 apps, and — the
layer added most recently — which apps carry which component.

**The knowledge pool** (`knowledge-memory`, ADR-0084) is *accumulated*. It cannot
be recomputed from anything, because it is history: what a run tried, what the
gate said, which lesson a branch read, whether that branch passed. Its retrieval
is dense-plus-lexical over one string.

An `entry` is:

```wit
record entry { ns, key, text, goal, env, attempt, score }
```

There is no structural key in it. A lesson is attached to **a sentence** — the
goal it was learned under — and found again by cosine similarity to another
sentence.

## Why that is not good enough

Both paid runs this repository has made in its current shape failed on facts of a
kind that similarity retrieval is bad at:

* `Dialect.delimiter` is a `String`, not a `char`.
* serde is built here without `std`, so `HashMap` has no `Serialize` impl and
  `json!({"by_vet": map})` will not compile.
* `records::find_by` only finds fields that were passed in `index_fields` at
  create time.

None of these is a fact about the clinic. Each is a fact about an **interface** —
`csv:codec/codec`, the workspace's serde configuration, `records:store/store` —
and each will be true for every future user of that interface, whatever their goal
says. A goal about a booking system and a goal about a veterinary clinic have low
textual similarity and identical exposure to `records:store/store`, which has 37
consumers and ships in 39 apps.

Retrieval by wording finds the lesson when the wording happens to rhyme. That is
the wrong index for a fact about a contract.

## The join

**Tag a lesson with the capabilities the work touched, and retrieve by those as
well as by text.**

The tags are not authored, guessed or asked for — they are read out of the
capability graph, which already knows them:

```
part `reports`  →  writes components/clinic-domain/src/reports.rs
                →  component `clinic-domain`
                →  imports  csv:codec/codec@0.1.0
                            records:store/store@0.1.0
                            id:generate/generator@0.1.0
                            auth:identity/{accounts,session,types}@0.1.0
                            search:index/index@0.1.0
                →  composed into app `clinic`
```

`plug::Catalog` produces that in milliseconds, in-process, from the artifact. The
component is derivable from the part's own `writable` path; the app from the
closure. Nothing new has to be maintained, which is the point — a tag a human
types is a tag that goes stale.

Three keys per lesson, then: the **interfaces** the part actually imports, the
**components** its app composes, and the **app** being built.

## What the edge buys

**Retrieval stops depending on wording.** `recall` becomes "lessons similar to
this goal, UNION lessons tagged with an interface this part imports". A part that
imports `csv:codec/codec` is handed what the last part to use it learned, even
though one was writing a clinic and the other a billing ledger. This is the
difference between a pool that helps the same app twice and one that compounds
across apps.

**Promotion gets a weight it currently lacks.** A lesson about an interface with
37 consumers in 39 apps is worth more than one about an interface with a single
consumer, and today nothing knows the difference. The graph does. Same signal for
diversity: a branch reading about a load-bearing interface is reading something
likely to matter.

**Decay gets a structural rule.** Today a lesson is forgotten by age and unread
count (ADR-0084). Add: a lesson tagged with an interface that is *no longer in the
catalogue* is dead weight regardless of age, and a lesson tagged with a version
that nothing imports any more describes a contract nobody builds against. That is
garbage collection the pool cannot currently do, because it has no idea what an
interface is.

**Discovery gets a second signal** (ADR-0089). Lessons mentioning
`search:index/index` are evidence that a searching capability exists and evidence
of how it is used wrongly. A capability search that reads both the catalogue and
the pool answers "what do we have, and what is known about living with it".

**"What did we learn building this app?" becomes a query.** It cannot be asked at
all today.

## What NOT to do

**Do not merge the two stores.** It is tempting — both are graphs, and SurrealDB
is already there. It would be a mistake. The capability graph is derived and must
stay derived: its whole value is that it cannot be stale, and the moment it lives
in a database that also holds history, somebody will write to it and it becomes a
second copy that can disagree with the artifacts. The knowledge pool is the
opposite: it is history, and history is exactly what must not be recomputed.

Keep them apart and **join by key at query time**. The tag is a string like
`csv:codec/codec@0.1.0`; the catalogue is the authority on what that string means
today; the pool is the authority on what happened to somebody who used it.

**Do not let an agent write its own tags.** The same rule as promotion (ADR-0084):
a tag decides what future runs are shown, so it comes from the artifact, not from
the model that wants to be found.

## The first slice

1. `entry` gains `tags: list<string>`, `recall-opts` gains `tags: list<string>`.
   Stored beside the text; matched exactly, no embedding involved.
2. `recall` returns text-similar hits UNION tag-matched hits, with the existing
   outcome weighting applied to both. `k = 0` stays the control arm.
3. `goalrun` derives the tags from `plug::Catalog` for each part and passes them on
   both `observe` and `recall`. It already links the library, so this is a few
   lines rather than a new dependency.

Roughly 150 lines across `memory.wit`, `knowledge-memory`'s `lib.rs` and `surql.rs`,
`reconciler/src/memory.rs` and `goalrun`. The honest cost is not the code, it is
that `knowledge-memory` has 20 tests pinned to live SurrealDB behaviour and the
statements need to keep passing all of them.

## How it would be proven

Not by a green test that asserts the wiring. The claim is that a lesson learned in
one app reaches a different app that shares an interface, so:

> Two goals with deliberately dissimilar wording, both importing
> `records:store/store`. The first fails on `find_by`'s indexing rule and writes
> the lesson. The second is run twice — once with tag retrieval, once with text
> retrieval only — and the tagged arm gets the lesson while the text arm does not.

That is the same shape as `learning.rs`, which proves the existing loop with a
scripted model and no AI spend, and it is falsifiable: if similarity already finds
the lesson, the control arm passes too and this ADR is wrong.
