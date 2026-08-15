# ADR-0085 — structure flows down, lessons flow up

*A second knowledge pool: what the codebase IS, as a graph, shared by content
address between the branches of a goal — and why it must not live in the pool
that holds what the swarm learned.*

**Status: proposed. Nothing is built.** ADR-0084 built the lessons pool and wired
its first slice; this is the design for the other half, written before any code
because the isolation question in it is not mine to answer alone.

## The gap

`knowledge:memory` answers "what did we learn". Nothing answers "what is this
codebase". A branch handed a goal about a search box has to rediscover, every
time and from the prompt alone, that the route is served by one handler, that the
handler shares a type with the frontend, that the type is defined in a third file
nobody named. Twenty branches rediscover it twenty times, badly, and the parts
they get wrong are the ones no test covers.

ADR-0081 called this the third retrieval layer and left it: *"only the third needs
`knowledge:graph`"*. `knowledge:graph/neighbours` — one hop, the thing an index
over flat records cannot do — has been built and tested since ADR-0080 and
**nothing populates it**.

## Why a second pool and not a bigger one

The tempting move is one pool with a `structure` namespace. It is wrong, because
the two obey opposite rules in every dimension that matters:

| | lessons (`knowledge:memory`, built) | structure (this ADR) |
|---|---|---|
| a row is | "approach X failed check Y" | "route `/api/search` is served by `handler::search`" |
| its truth | episodic, sometimes wrong | factual, or the parser is broken |
| written by | an agent (observed) or a gate (promoted) | **derived** — no model in the path, so no poisoning to defend against |
| ages by | outcome weight; decays when runs that read it fail | **not at all** — invalidated by a *diff*, never by time |
| keyed by | hash of the normalised goal | `(repo, commit, path)` — content, not intent |
| retrieved by | similarity, fused, budgeted | **traversal**, one hop from the files the goal names |
| flows | **up**: a branch's outcome informs its siblings and the next generation | **down**: a subgraph inherits its parent's index unchanged |

The last row is the one that decides the architecture. Lessons are contested —
that is why they are outcome-weighted, why `patterns` is gated, and why the
component that holds them is a policy layer. Structure is not contested: two
branches disagreeing about which file defines a symbol means one of them has a
bug. Putting facts through machinery built to distrust its writers costs
complexity and buys nothing; putting lessons through machinery that assumes its
writers are right is how a pool gets poisoned.

So: a second component, `code:index`, and the same graph underneath.

## The shape

Nodes, all keyed within a commit:

    file    path
    symbol  path + name           fn, struct, class, type, component
    route   method + path         "GET /api/search"
    literal a shared string       a route, a schema name, an event name

Edges:

    file   -defines->  symbol
    file   -imports->  file
    symbol -calls->    symbol
    route  -served-by->symbol     the backend handler
    symbol -requests-> route      the frontend call site
    symbol -mentions-> literal

`served-by` and `requests` are the pair that answers the question this ADR exists
for. A frontend that calls `/api/search` and a backend that registers it are two
files with no import between them, in two languages, that no similarity search
will ever put next to each other — and one hop apart in a graph.

### Where the cross-cutting edges come from: shared literals, not comprehension

The mechanism is deliberately stupid. A route path is a **string constant that
both sides must spell identically or the software does not work**. So the indexer
does not need to understand either language: it collects string literals, keeps
the ones shaped like a route (or a schema name, or an event name), and draws an
edge between every symbol that mentions the same literal.

That is a language-agnostic join over the one thing the two ends are contractually
obliged to agree on. It finds the FE↔BE pair, the publisher↔subscriber pair, and
the migration↔model pair, with no parser that knows what a framework is.

It also fails in a known way: a route assembled at runtime (`` `/api/${kind}` ``)
has no shared literal and no edge. That is a real hole and it is better than a
framework-aware parser per stack, which is a hole per framework we did not write
yet.

## Derivation, not reporting

Everything above is computed from the tree. Three consequences worth stating,
because each removes a mechanism the lessons pool needed:

- **No write policy.** Nothing an agent says reaches this pool, so there is no
  trusted/untrusted split, no promotion, no gate.
- **No decay.** An entry stops being true when the code changes, which is an
  event the platform can see: `vgit:store/worktree/diff(before, after)` already
  answers which paths changed between two commits. Reindex those; leave the rest.
- **No outcome weighting.** A fact does not get better because a run passed.

### The parser, and its ceiling

There is no tree-sitter and no `syn` in this workspace, and adding a grammar per
language to a wasm component is a large dependency for a first cut.

**Recommended: a line scanner with a per-language pattern set** — declarations,
imports, route registrations, string literals — producing `file`, `symbol`,
`imports` and `literal` at maybe 80% recall. It is a few hundred lines, no
dependencies, and every edge it draws is checkable against the file it came from.

`ponytail:` regex-shaped extraction, per language. Upgrade to a real parser when
retrieval quality is measurably limited by *missed* symbols rather than by ranking
— and measure that before paying for it, because the failure everyone assumes
(bad recall) is rarely the one that hurts.

## Shared by content address, which is what makes it affordable

The index is a pure function of a tree. Twenty branches of one generation share
one base commit, so they share one index — and `artifact:cache` exists for exactly
this, says so in its own header, and is unused by the loop:

    artifact-key { producer: "code-index", version: <indexer version>,
                   inputs: [<commit>], params: <language set> }

`derive-id` names it before it exists; `lookup` answers `hit` / `claimed` /
`pending`, so the first branch to ask computes it and the other nineteen wait or
get it free. The **version is in the key**, so an indexer that changes its rules
produces a different artifact rather than silently serving incompatible edges.

This is the "flows down" property in one sentence: a child environment derives
from a parent commit, so it addresses the parent's index without copying it, and a
branch that changes nothing pays nothing.

## The open question I am not answering alone

**A store is named after its app (ADR-0023), and a branch is its own derived app
(ADR-0078). So where does a shared index live?**

Content-addressed sharing needs a store that several apps can read; isolation says
they cannot. Three ways out, none free:

1. **The parent owns it, children link to the parent's cache.** Cheapest, and it
   punches a hole in exactly the boundary that makes branches safe. A link a
   tenant could author is a link a tenant could use to escape its box (ADR-0008),
   so it would have to be stamped by the platform for derived apps only.
2. **The reconciler holds it** and injects the relevant slice into each branch's
   plan. No cross-app store at all, and the graph becomes something native code
   passes around rather than something a component queries — which loses the
   traversal at exactly the moment a branch wants a second hop.
3. **The index is written into `vgit` as an object** under the commit it describes.
   Content-addressed by construction, but a branch's `vgit` is a branch's, so this
   only shares if the object store is shared — the same question one level down.

`.comp/goals/03` marks this thread human-led for this reason. My preference is (1)
with the link stamped by the platform and readable-only, but it is a security edge
and it should be decided deliberately.

## Retrieval, and where it joins what already exists

Layer 3 from ADR-0081, and it does **not** need a new retriever in
`knowledge:memory`:

    goal ─▶ files named in the goal ─▶ 1 hop over defines|imports|served-by|requests
                                    ─▶ structurally related files
                                    ─▶ prepended to the prompt as CONTEXT, not as advice

Two rules it inherits and one it adds:

- it comes out of the same **character budget** as retrieved lessons, or the
  diversity budget stops meaning anything;
- it is labelled as structure in the prompt. A fact and a lesson read differently
  and a model that cannot tell them apart will hedge about both;
- **one hop, not two.** Two hops from a busy file is most of the repository, and a
  context window full of "related" is a context window with no room for the goal.

## What this does not do

- **No semantics.** It records that two things mention `/api/search`, not that one
  calls the other correctly. It is a map, not a type checker.
- **No cross-repo edges.** One repo, one commit, one index.
- **Nothing for non-code assets** — schemas, migrations, OpenAPI documents — beyond
  whatever literals they happen to share.
- **No measurement yet.** The claim that structural context improves a branch's
  score is exactly that: a claim. It wants the same treatment as the shared pool —
  a control arm, and a generation where some branches get structure and some do
  not (ADR-0081's diversity budget already provides the shape).
