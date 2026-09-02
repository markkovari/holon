# ADR-0097 — A derived file is a liability, and a name is not a fact

*Two things the tree kept doing: writing down what it could compute, and deciding
what something IS from what it is CALLED. Both were measured, and both were wrong in
ways nobody could see from reading.*

**Status: accepted**, and done. Supersedes the mechanics of
[0089](0089-capability-accumulation.md) §"The pool exists" and
[0094](0094-a-capability-describes-itself-in-a-callers-words.md) §"where the sentence
lives", both of which describe `catalog.json` and `tools/gen-catalog.py`. The claims
those ADRs make still hold; the files they name are gone.

## A derived file is a liability

`components/catalog.json` was 500 KB, generated, committed, and read by `capsearch`
to answer two questions about a component. It carried `wasm_size_bytes` and
`wasm_sha256_12` straight from the last build, so it was **stale by construction** —
the moment anyone ran `just build` it disagreed with the tree, for reasons that had
nothing to do with the catalogue. That is also why it never had a staleness guard:
one was impossible.

Taking the build output out made it checkable. Checking it made the real problem
visible: once `capsearch` asked the components directly, **the only things left
reading the file were the tests checking whether it had gone stale.** A file that
exists to be verified rather than used is a liability with a guard bolted on. The
guard exists because the file does.

It was committed "for tooling". There was no tooling.

`comp-catalog --json` computes the same answer from the components in about a
second. `components/CATALOG.md` survives for the one reason nothing else here has:
**people read it on GitHub without running anything.** A rendering for humans is not
a cache for programs, and it keeps its guard, because a rendering can still lie.

**Two renderings survive on that reasoning, not one.** This ADR originally called
`CATALOG.md` the only derived file still committed and `reconciler/tests/derived.rs`
repeated it; `docs/CAPABILITY-GRAPH.md` is the other, and it has its own staleness
guard in the same file. The claim was wrong when written. Adding one component makes
both stale, the two guards fail one at a time, and #201 duly regenerated the graph,
missed the catalogue, and had CI report it a commit later. `just derived` runs both.

Their totals also differ — 215 against 213 at the time of writing — because
`comp-catalog` counts source crates while the graph counts what was BUILT, reading
each component's real imports out of its binary. The gap is the crates that declare
their own `[workspace]` and are never built. Both numbers are right, and the graph now
says so in its own header rather than leaving a reader to reconcile two derived files
by hand.

The same argument retired `tools/gen-catalog.py`: the catalogue is load-bearing —
[0089](0089-capability-accumulation.md) rests on `capsearch` finding what the pool
already has — and it had no test because it could not have one.

## A name is not a fact

`reusable_as_is` decided whether a component was a capability or an application. It
was `name.ends_with("-domain")` plus a hand-kept list of ten exceptions, and
`capsearch` used it to stop a showcase outranking the capability it is built from.

Measured against the components themselves, **the name was wrong 33 times out of
212, in both directions**:

* 30 were advertised as reusable while exporting nothing but
  `wasi:http/incoming-handler`. Nothing can plug a door. Every probe was in there,
  and so were `eshop-basket`, `-catalog`, `-gateway`, `-ordering` and `-payment` —
  five parts of ONE application, each offered to a goal as a capability.
* 3 were hidden from search while exporting real contracts: `login-app` exports
  `login:app/auth`, `reddit-domain` exports `local:reddit/reddit`, `power-domain`
  exports a bare `calculate-cost`.

**There is no "application" in this tree any more.** In the component model
everything is a component; "application" was our word, and it existed for one job.
That job needs a property, not a category:

    a component offers a contract when it exports something outside `wasi:`

which is what `wac plug` can satisfy, read off the component, needing no convention
and no exceptions to the convention.

## What actually found these

Not reading. Three bugs in the source-side extraction survived review and were each
caught by one test — that `comp-catalog` (reading SOURCE) and `capsearch` (reading
ARTIFACTS) must agree on all 212 components:

1. `world calc { export arith; }` is one line, and the pattern was anchored to the
   start of one. Seven components had no exports recorded at all.
2. Un-anchoring it matched the word "export" inside prose, capturing *"and a
   PII-redacted audit view — every cross-cutting concern…"* as an interface name.
3. A crate may target the shared repo-root `wit/`, so reading every world there
   attributed an unrelated `types` interface to two components.

The generalisation is [0087](0087-a-composition-is-derived-not-written.md)'s, one
level up: **two independent derivations of one fact, required to agree, find things
neither review nor a single derivation can.**

## The rule

Before committing a generated file, ask what reads it. If the answer is "the test
that checks it is fresh", delete it and keep the program. Before writing a rule that
inspects a name, ask what the name is standing in for, and whether that thing can be
read instead.
