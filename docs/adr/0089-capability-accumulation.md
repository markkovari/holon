# ADR-0089 — capability accumulation: reuse before build, promote what generalises

*Every solved problem should leave the system able to do more than it could before.
An agent that rediscovers PDF parsing on Tuesday because Monday's environment was
thrown away is not learning; it is paying twice.*

**Status: proposed, and roughly half of it is already built — which is exactly why
it is worth writing down.** The parts that exist are named below with what proves
them. The parts that do not are named with equal precision, because the gap between
"we have 150 reusable components" and "an agent finds and reuses them by itself" is
where all the remaining value is.

## The claim

A WASM component is **executable knowledge**. That makes it categorically more
valuable to an agent than a note saying "you can use a PostgreSQL client", and it
suggests a rule the loop does not yet follow on its own:

> Before generating an implementation, look for a component that already provides
> the capability. Reuse it. If nothing fits, build it — and if what you built
> generalises, promote it into the pool.

ADR-0084 already separates the two kinds of memory: **knowledge** is what a run
learned (lessons, in `knowledge-memory`, with promotion and decay), **capability**
is what the system can execute (components, in the catalogue). This ADR is about
the second one compounding the way the first already does.

## What is already true

Stating this plainly because a proposal that ignores what exists tends to
re-propose it.

**The pool exists.** 150 built components, and `components/catalog.json` records
109 of them with `exports`, `capability_deps`, `config_keys`, `wasm_sha256_12` and
a human-authored `reusable_as_is` flag. `tools/gen-catalog.py` generates it.

**Capability lookup by interface exists.** `plug::Catalog::exporter(iface)` answers
"what can satisfy this import", and `wit-reflect`'s `satisfies` answers the harder
version with `wac`'s own subtype checker — an export whose type does not fit does
not fit, whatever the names suggest.

**The capability graph is queryable, crudely.** `reconciler/tests/contracts.rs`
reads all 150 components from both sides: 93 interfaces exported, 80 consumed
in-tree, `records:store/store` carrying 37 consumers and `auth:identity/authorizer`
19. That is the "who uses what" view the pitch asks for, minus the richer edges.

**Reuse is ENFORCED where it has been asked for.** This is the part that surprised
me. The clinic's `access-and-search` part is judged by a gate that reads the
compiled component's imports and fails a candidate that hand-rolled password
hashing or ranked search instead of calling `auth-guard` and `search-index` — a
thing no behavioural test can detect, since both answer 200. A real model passed it
at 1000 on its first generation, reaching for three capabilities it was not handed
code for. `reports` is the same shape around `csv:codec`.

So the rule is already provable, per goal, when a human writes it into the world.

## What is missing, precisely

**1. Discovery. The agent does not search; a human writes the world.** In the
clinic, *I* put `auth:identity`, `search:index` and `csv:codec` into
`wit/clinic.wit`. The part then had no choice but to use them. That is enforcement
without discovery, and it does not compound: every new goal needs a person who
already knows what the pool contains.

What is needed is a step before generation: given a goal, query the catalogue for
capabilities that plausibly apply, and put them in the part's world automatically.
The pieces exist — descriptions in `catalog.json`, an embedding path in
`knowledge-memory`, similarity search already used for lessons — but nothing wires
them together.

**2. Promotion. Nothing a run builds ever enters the pool.** A component the loop
writes stays in the app it was written for. There is no step that asks "does this
generalise?" and no path from a passing candidate to `components/<new>/`. Today the
answer is a human deciding, which is the same bottleneck as (1).

**3. Duplicate detection.** Without it, promotion produces the twelfth PDF parser.
The pool already has near-neighbours worth noticing — `cache` and `cache-backing`
export different interfaces of the same package; `anthropic-provider`,
`openai-provider` and `ai-inference` overlap deliberately. A promotion step needs
to distinguish "a new capability" from "a worse copy of an existing one", and the
similarity machinery for that is the same one the knowledge pool uses on lessons.

## Two things the framing gets wrong for this repository

Recording these because adopting the idea wholesale would import them.

**"Improve the existing component instead of duplicating" collides with frozen
interfaces.** `records:store/store` has 37 consumers. Improving it in place is not
a local edit; it is a migration, and an agent that "extends" it because its goal
needed one more method has just changed something under 37 apps that no gate in
this repository runs. Extension has to mean a NEW interface, or a new version with
both live — which is what `tests/contracts.rs` already watches for, since an import
of `@0.1.0` is not satisfied by an export of `@0.2.0` and `wac` will not say so.

*Both live* is the operative phrase, and it is a storage requirement rather than a
policy one: **components are keyed, and both versions stay resolvable.** The
ingredients exist — `catalog.json` records a `wasm_sha256_12` per component, and
`plug::compose_to` already writes content-addressed artifacts, so the same
component at two versions produces two files that cannot collide. What is missing
is that the KEY is currently the crate name. It should be the pair the resolver
actually needs — the exported interface with its version, and the digest of the
artifact that provides it — so that `records:store/store@0.1.0` and `@0.2.0` are
two entries rather than one entry that changed. Then a consumer pinned to the old
one keeps composing while a new consumer takes the new one, migration becomes a
per-consumer move instead of a flag day, and nothing has to be frozen by social
convention.

**"Promote every reusable artifact" is the wrong default.** Promotion has a cost
that is invisible at promotion time and permanent afterwards: a component in the
pool is something a later agent will find, trust, and build on. Promoting an
under-tested one is worse than not promoting at all, because the failure lands on
somebody who did not write it. The bar should be the one the loop already applies
to a *lesson*: a candidate may only promote what a gate proved (ADR-0084 —
`promote` is refused when the score did not pass, and it is deliberately not
reachable from an agent's own world).

But the *duplicate* half of the decision should not be a judgement at all —
**`wac` decides it.** "Does this generalise?" and "is this the twelfth PDF parser?"
sound like questions for a model and are not: `wit-reflect`'s `satisfies` runs
`wac`'s own `SubtypeChecker`, which answers exactly whether a candidate's exports
satisfy an existing interface. That gives a mechanical rule with no taste in it:

| `satisfies` an existing interface? | what it is | what happens |
| --- | --- | --- |
| yes, fully | another implementation of a capability we have | do not promote as new — register as an alternative provider, keyed by digest |
| partially | a near-duplicate | refuse, and say which interface it nearly fits |
| no | genuinely new capability | promote, under its own key |

The same check answers "can this be swapped in?" for a consumer, so discovery and
promotion end up using one mechanism rather than two heuristics. An interface a
candidate merely *names* the same way is not a match, which is the failure mode a
name-based registry has and a subtype-based one does not.

## The first slice, if this is taken up

Ordered by what unblocks the most with the least new machinery:

0. **The graph.** *Done, since this ADR was written:* `comp-capgraph` derives
   who-imports-what-from-whom from the built artifacts and writes
   [`docs/CAPABILITY-GRAPH.md`](../CAPABILITY-GRAPH.md) — 150 components, 80
   consumed interfaces, 300 import edges, and the number that decides whether an
   interface may change at all. `just capgraph` regenerates it and a test fails
   when the committed copy goes stale. This is the substrate the three steps below
   query.
1. **Catalogue query as a component.** `capability:find/search` over
   `catalog.json` — "what exports an interface like this?" and "what does this
   description match?". `knowledge-memory` already does embeddings and KNN over
   SurrealDB; this is that, over component descriptions rather than lessons.
2. **Discovery in the planner.** Before a decomposed goal writes its parts' worlds,
   ask (1) and add what it finds. The gates that enforce reuse already exist, so
   discovery immediately becomes testable: the part's world grows a capability the
   human did not name, and the import check proves the part used it.
3. **Promotion, gated.** A candidate that passed, whose new code exports a WIT
   interface, that is not a near-duplicate of something in the pool, becomes a
   component in its own directory with its own gate. Refused otherwise, and never
   reachable from the agent's own world — the same shape as lesson promotion.

Only (3) is genuinely new. (1) and (2) are wiring between things that already work.

## Consequences if it lands

* A goal's cost falls over time rather than staying flat, because the Nth app
  composes what the first N−1 left behind.
* The economic argument sharpens: a cheap model is enough when the capability
  already exists, and the expensive one is only needed for genuinely new
  capability. That pairs with the model-per-branch gap (goal 03), which is still
  open — an environment is a copy of its parent, so a generation cannot put a cheap
  model on three branches and an expensive one on the fourth.
* The honest failure mode to watch: a pool that grows faster than its gates. 150
  components with 93 exported interfaces is already more than any one person holds
  in their head, which is the argument for (1) and simultaneously the risk in (3).
