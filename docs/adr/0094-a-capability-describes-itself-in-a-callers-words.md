# ADR-0094 — a capability describes itself in a caller's words

*A searcher is only as good as the sentences it searches. Half of ours said
nothing.*

**Status: accepted**, and built. Completes the *discovery* third of
[ADR-0089](0089-capability-accumulation.md).

## The measurement that started it

`comp-capsearch` works: term overlap over the catalogue, no model, a millisecond,
with the graph breaking ties towards what applications already carry. Its own
source recorded the problem it could not solve:

> 57 of the 109 entries say only "`x` — reference implementation of `x:y`", which
> is a tautology and matches nothing a caller would type.

That is not a retrieval-architecture problem. It is a **content** problem wearing
a retrieval problem's clothes, and no index, embedding or synonym list fixes a
corpus that describes each component by restating its own name.

The clearest case: `rate-limiter` ships in 22 applications, and the question
*"stop a caller making too many requests"* did not find it — because the words it
used were "counts failures against an opaque key". That query was in the test
suite, as a **known miss**, with a note saying it was the trigger for the
embedding path.

## The decision

**A component's first `//!` line says what a caller wants, not what the component
is.** It is generated into `catalog.json` by `tools/gen-catalog.py`, so that line
— in the source, where a person writes it — is the searchable one.

Enforced by a lint that refuses one form: describing yourself as a reference
implementation of your own interface. Deliberately blunt. It does not judge
whether prose is *good*, because a rule that scored prose would be argued with,
and this one cannot be: restating your own name is not a description.

The WIT header below that line stays as technical as it likes. `capsearch`
already reads it as a second field, and reading it was never enough — the 52%
measurement was taken *with* the WIT prose in place.

## What it bought

57 descriptions rewritten, plus a 58th the catalogue check had missed: the lint
reads the source and caught `auth-guard`, whose wording was
"Reference implementation of the `auth:identity` contract" — **the most consumed
capability in the repository**, with 19 importers, unfindable by anybody who did
not already know its name.

The known miss now passes. Nothing about the searcher changed:

| question | before | after |
| --- | --- | --- |
| stop a caller making too many requests | *nothing* | `rate-limiter` |
| record who did what and when | — | `audit-log` |
| make a mutating request safe to retry | — | `idempotency-guard` |
| mask personal data before it reaches a log | — | `pii-redact` |
| the books have to balance | — | `ledger` |
| do this later, on a timer | — | `scheduler-timer` |

Eight questions were added to `SHOULD_FIND`, every one of which failed before.

## The searcher's own defect, found by fixing the corpus

*"two workers must not do the same job"* matched **57 of 152** capabilities and
ranked `outbox` first, scoring on "two", "not" and "same" — three words that say
nothing about what anything does. `STOPWORDS` existed and did not contain them.

With those removed, the query still misses. The cause is structural and is
recorded rather than tuned around: **a match on a name or an interface scores 3, a
match on a description scores 1**, so any query using a word that appears in some
component's name outranks a component that does the thing. `jobs-domain` beats
`lock-mutex` for "job".

It bites because the pool contains **33 domain applications**. Nobody reuses
`jobs-domain`; they reuse `lock-mutex`. An application is not a capability, and
having both in one pool is the actual fault. The fix is the app-local tier —
applications are not in the catalogue at all — not a synonym list and not a weight
somebody tuned until this one query passed.

## Discovery is now asked, not offered

`comp-goalrun` searches the pool with the goal's text before a branch spawns,
prints what it found, records `capsearch-hit` or `capsearch-miss`
([ADR-0092](0092-a-run-leaves-a-trace.md)), and puts up to five candidates into
every branch's context as `POOL.md`.

Mandatory rather than advisory, because **the answer is the point in both
directions**. A hit is reuse a branch would otherwise have missed. A miss is the
graph naming a capability the pool lacks — the only corpus that answers "what
should we build next" — and it accumulates only if the question is asked on every
run, including the ones where nobody expected an answer.

It is prose in the context, not an instruction. The gate decides whether reuse
happened, and a branch *told* to reuse something that does not fit would do it
badly.

## Honestly: not on every path

**Decomposed runs are not traced at all.** `decomposed()` has no `Trace` — no
`run_started`, no events, nothing — so a multi-part run (ADR-0086) leaves no
record and its capability search is neither run nor recorded. That is a
pre-existing hole in ADR-0092's implementation rather than something introduced
here, and it is stated because "mandatory" would otherwise be a claim this
codebase does not honour.

## Cost, and what is not claimed

The search adds one catalogue scan per run — a millisecond of term overlap, and no
model. Nothing is blocked on the result: a run whose search found nothing is a run
that proceeds, with a row recorded saying so.

This does not make discovery *good*. It makes it asked, recorded, and searching a
corpus that contains sentences. Two questions still miss, both listed, both with
the same cause, and the fix for both is a decision this ADR does not make.
