# ADR-0086 — parts negotiate a contract

*A goal that diverges into a frontend and a backend: the interface they must both
build against does not exist yet, they are allowed to ask each other for changes to
it, and neither may block on the other.*

**Status: built and demonstrated end to end.** A decomposed goal runs on a real
fleet — eleven components across three apps, a real SurrealDB, the real native
checks runner — and `reconciler/tests/compose.rs` watches a frontend ask for a
field, a backend grant it, the backend demonstrate it, the amendment ratify, and
both halves compose into one tree that passes a gate neither part could pass alone.
`holon goal run` asks for it with `[[part]]` in the goal spec, and
`--smoke` proves the whole decomposed deployment without spending anything. `components/contract-registry` owns versions, the
request protocol and the composition check (13 tests, every statement verified
against a live SurrealDB v3.1.3); `components/contract-probe` gives it an HTTP
surface, the same shape `driver-probe` has; `generation::compose_search` runs K
parts concurrently, generation by generation; `contract::Registry::boundary`
ratifies what has been demonstrated and answers what is outstanding with one cheap
model call per request; `compose::run_parts` is the whole loop — ask,
answer, ratify, merge, gate — and `holon goal run` is a caller that prints and
lands, so the composed e2e drives the binary's own orchestration rather than a
re-spelling of it.

ADR-0085 is the brownfield half — indexing an application that exists. This is the
other tense: two subgraphs building complementary halves of something that does
not exist, which will each invent their own `UserDto` unless something stops them.

## What changes underneath

Every branch so far has been a **competitor**: N attempts at one goal, judged
against each other, one winner landed (`generation::search`, `best_of`, `land`).
A decomposed goal is not that shape. Its branches are **parts** — complementary,
all of them needed, and a generation that produces a brilliant backend and no
frontend has produced nothing.

    goal ──▶ parts: [backend, frontend]        each part is a holon: a whole,
              ├─ backend  → N competing branches → winner    and a part of a
              └─ frontend → N competing branches → winner    larger whole
                        ↓
                 compose → one PR

So competition is now *inside* a part and composition is *between* parts. The
selector picks a winner per part; something new joins them.

## What a person writes

```toml
text     = "Add a paged search box: a backend route and a frontend that renders it."
contract = "CONTRACT.json"          # the interface, as a file they can edit

[[part]]
name     = "backend"
text     = "Serve GET /api/search over the corpus, exactly as CONTRACT.md describes."
writable = ["src/api.rs"]
[[part.check]]                       # this half's own gate: it runs against the
id       = "backend-serves-the-route"#  contract alone and waits for nobody
command  = ["grep", "-q", "/api/search", "src/api.rs"]

[[part]]
name     = "frontend"
text     = "Render the results with a pager, against the fixtures in .contract-mocks."
writable = ["ui/app.ts", "CONTRACT-REQUEST.md"]
[[part.check]]
id       = "pager-renders"
command  = ["grep", "-q", "pager", "ui/app.ts"]

[[check]]                            # with parts, the TOP-LEVEL checks become the
id       = "the-join"                #  composition gate — the whole, not a half
command  = ["grep", "-q", "total_pages", "ui/app.ts"]
```

Three decisions are in that file rather than in code. Each part carries **its own
checks**, because a half that gates against the contract alone is a half that never
waits. The top-level `[[check]]` list becomes the **composition gate**, because the
checks that belong to the whole are exactly the ones neither half can run.
`CONTRACT-REQUEST.md` is in the frontend's `writable`, because a part that may not
write the file cannot ask for anything.

No parts means the ordinary path, unchanged: one goal, N competing branches, one
winner. A goal with parts and no `--surreal-url` is refused up front — the registry
keeps versions and the negotiation history in a database, and nothing here deploys
one (ADR-0080).

### What `--smoke` proves, for free

A decomposed run brings up five apps, and every one of them can be checked without
a model call or a pull request — an app whose secret cannot be granted or whose
egress is malformed never serves at all, so reaching the end is itself the result.
Run against a two-part goal with **fake keys** and a local database:

```
SMOKE OK:
  · both graphs started and serve → links, egress and secret GRANTS are correct
  · driver reachable → {"error":"invalid","detail":"max-attempts is zero, …"}
  · the contract registry and the answer door serve
  · contract v1 published from CONTRACT.json → registry → graph → SurrealDB,
    and the database's secret was granted
  · the answering model is reachable and says it is → {"provider":"claude-haiku-4-5-…"}
  · parts: backend, frontend

What smoke does NOT check (needs a real call, costs money):
  · that the Anthropic key VALUE is accepted
  · that the GitHub token VALUE can open a PR
  · that the parts negotiate — the first request costs one small call
```

Publishing is the load-bearing line: it exercises the probe, the registry, the
graph, the egress allow-list, the vault secret and a real SurrealDB in one call. A
second smoke run finds the contract already there, which proves the same chain and
is reported as such rather than as a failure.

## The contract, and why the human writes the first one

A person writes the goal and the checks. **A DTO contract is a check** — routes,
request and response shapes — so it belongs in the goal spec beside `writable` and
`[[check]]`, authored by the person who already had to describe the work.

But a broad goal cannot be specified to the last field in advance, and pretending
otherwise produces a contract that is wrong by attempt two. So the first contract
is a **starting point with authority**, not a complete specification: it is what
the parts build against until one of them asks for a change.

## Parts may ask each other for things

A part may send a **request** to another part. The other part may grant it, deny
it, or answer with something better.

    request  frontend → backend: "SearchResult needs `total_pages`; I cannot
                                  paginate from `next_cursor` alone"
    answer   granted   → the contract amends, version bumps
             denied    → with a reason, which the asker must read
             counter   → "use `has_more`; total pages costs a COUNT on every query"

The counter is the interesting one and the reason "deny" alone is not enough. The
part being asked knows something the asker does not — that is the whole point of
having diverged — and a protocol that can only refuse throws that knowledge away.

**A request is not a message to a running process.** There is no mailbox and no
actor: a part is a branch that may already have finished. A request is a row
addressed to a *part*, and answered at the next generation boundary by a cheap
model call carrying that part's plan and current candidate. No runtime, no
delivery guarantees, nothing to wedge.

## Nothing blocks inside a generation

This is the liveness rule, and it is the one thing in this ADR I would not trade:

> **Requests are asynchronous; resolution is synchronous at the generation
> boundary.**

Within a generation, every part builds against the contract version it started
with. Requests accumulate. At the boundary, all outstanding requests are resolved
together, the contract bumps a version, and the next generation starts from it.

Two branches waiting on each other mid-generation is the deadlock this design
exists to make impossible, and it is impossible because there is nothing to wait
on. The generation boundary is a clock that already exists (`search`'s round loop),
and `max_rounds` is already the bound on how many times this can go round.

The cost is honest and worth stating: **a needed change costs a generation.** The
frontend that discovers it needs a field spends the rest of its generation without
it. That is the price of never blocking, and the alternative — synchronous
negotiation mid-flight — buys one generation of latency for an entire class of
deadlock.

## An amendment is a promotion, and the same rule applies

An agent may propose. Only a passing gate may promote (ADR-0084). Here that reads:

- a **granted** request produces a *proposed* contract version;
- it becomes **canonical** when the granting part's own gate passes against it —
  that is, when the backend has actually served `total_pages` and its checks are
  green;
- until then the other parts keep building against the last canonical version.

A part that can amend the shared contract at will is a part that can poison every
sibling, which is knowledge poisoning wearing a different hat. The defence is the
one already built: the gate, not politeness.

## Versions travel, and a mismatch is loud

Every part's output records the contract version it was built against, and **the
composition gate refuses to compose parts built against different versions.**

This is the same failure as an index holding vectors from two embedding models
(ADR-0084): two things that are individually fine and meaningless together. There
it was caught by a database constraint; here it is caught by a check, and it must
be a refusal rather than a warning — a frontend built against v3 and a backend
against v4 will compile, deploy, and fail in production on one field.

## Two green parts are not a green whole

Each part gates alone: the backend against contract tests, the frontend against a
**mock generated from the contract**. That is what lets them diverge without
waiting — the frontend develops against the contract itself, so it cannot drift
from something that has not been written yet.

Then a **composition gate** runs the winners together. It is a third set of checks
and it belongs to the goal rather than to any part. Everything interesting will
fail here: the parts that agreed on a shape and disagreed on what it means.

## One pull request, at the end

One PR for the whole, carrying every part's winner, the final contract version,
and **the negotiation history** — who asked for what, who refused, and why.

That last is not decoration. It is the part of the run a human reviewer most needs
and could never reconstruct: *the frontend asked for total pages, the backend
countered with `has_more` because the count cost a full scan, and that is why the
UI says "more" instead of "page 3 of 9".*

## What the registry turned out to need

Three things only running the statements could have said, in the tradition of
ADR-0080:

- **`ORDER BY` must project what it sorts by.** `SELECT subject … ORDER BY
  at_version` is a *parse error* — "Missing order idiom `at_version` in statement
  selection" — not a silently unsorted result. Both statements that sort therefore
  `SELECT *`, and a test pins that so narrowing the columns later cannot break the
  sort at a distance.
- **A guard belongs in the statement.** `UPDATE contract:⟨2⟩ SET canonical = true
  WHERE owner = "frontend"` matches nothing and returns an empty result, which is
  how `ratify` learns a part tried to ratify a version it does not own — no read,
  no race between the check and the write. `answer` uses the same shape with
  `WHERE answered = false`, and an empty result is how the second of two
  boundary passes learns it lost.
- **The version counter is `n += 1 RETURN n`.** Two parts granting amendments at
  one boundary is the normal case, and the read-then-write version of that was
  measured landing 7 of 60 concurrent writes (ADR-0084). The atomic form returns
  what it became, and `knowledge:graph` resends it if the transaction conflicts.

And one claim from this ADR is now demonstrated rather than argued: **"which
decision broke the join" is one traversal.**

    SELECT part, version, ->built_against->contract.from_request FROM build:⟨cand-be⟩
    → { part: "backend", version: 2, from_request: ["r1"] }

From a candidate to the contract version it was built against to the request that
caused that version to exist — which is the first time on this path that the graph
has earned its keep rather than merely being available.

## What this reuses, and what is new

Reused, unchanged:

- **`artifact:cache`** — contract versions and generated mocks, content-addressed,
  the version in the key. Every branch of every part shares one copy.
- **`comp:store/cas`** — two parts granting amendments at the same boundary is a
  concurrent write to one key; a conflict is a retry, measured at 60/60 (ADR-0084).
- **`knowledge:graph`** — the negotiation history is a graph and nothing else fits
  it: `request -asked-of-> part`, `answer -amends-> contract`,
  `candidate -built-against-> version`. "Which decision broke the join" is one
  traversal, and this is the first place on this path where the graph earns its
  keep rather than being available.
- **The gate**, as the arbiter of last resort. A contract nobody can implement
  fails every part, which is the correct outcome and needs no adjudication.

New, and each of these is a real cost:

- **A decomposer**: goal → parts. Human-authored first, because the same argument
  that puts the contract in the goal spec puts the split there. *(Unwritten.)*
- **A per-part `search`**, and a selector that picks per part rather than once.
  *(`generation::compose_search`, built.)* Three decisions inside it worth naming:
  a part that has passed its gate **does not run again** — re-running a solved part
  spends money to maybe make it worse; parts run **concurrently**, because
  sequential parts would make a two-part run as slow as both halves added together;
  and **a contract that moves un-accepts every part**, because a candidate built
  against v3 is not a candidate for v4 and pretending otherwise is exactly how two
  halves that each pass fail together.
- **The composition gate**, and mock generation from a contract.
- **The request protocol** and its resolution pass: one cheap model call per
  outstanding request per boundary. It is a spending decision and it belongs under
  the same budget as everything else, or negotiation becomes a way to spend money
  the budget cannot see.

## The contract is a file, not a field

`compose_search` lays the contract into each part's tree as `CONTRACT.md` and says
so in the goal text. Three things fall out of that and none of them needed a WIT
change: the writer already renders context files into its prompt, the model already
reads them, and `writable` already excludes it — so "you may read this and may not
edit it" is enforced by the same mechanism that enforces it for every other file.
It is also how a human would see it: one file both halves of the repository read.

## Answering, and why the parser is strict

One model call per outstanding request, carrying four things: what the asked part
was told to build, where it has got to, the contract as it stands, and the
question. Not its code — the question is about an interface, and a diff spends
budget on tokens that cannot change the answer.

The reply has exactly one legal shape:

    VERDICT: granted|denied|counter
    ---
    <the complete amended interface, or the reason, or the alternative>

Strict, because a free-form answer has to be *interpreted*, and an interpretation
of "well, that seems reasonable" that resolves to `granted` amends the contract
every other part builds against. Three rules fall out:

- **An unparseable reply answers nothing.** The request stays pending and is
  retried at the next boundary. A verdict invented from a reply nobody could read
  is a denial the model never made, and a denial is the one answer that cannot be
  taken back.
- **A grant must carry the whole interface**, not a description of the change. If
  the contract in force is valid JSON the amendment must be too — a model that
  answers `granted` with a paragraph has amended nothing, and storing the paragraph
  as the interface breaks every part at once. A contract that is not JSON imposes no
  such rule: the format is the goal's business, not the registry's.
- **A refusal needs a reason**, because the asker reads it and may ask again. A
  denial with no reason produces the same request next generation and the same
  denial after it.

The prompt says the quiet part out loud — *"grant only what you can actually
implement; if it costs you something the asker cannot see, counter with what is
cheap and say what it would have cost"* — because the counter is the answer that
makes divergence worth having, and a model that has not been told counters exist
will grant or refuse.

## What building it changed

Four things the design did not survive contact with, each found by the run rather
than by thinking:

- **The owner of a proposal must build against its own proposal.** Ratification
  means "I passed my gate against it", and every part was being handed the latest
  *canonical* version — so a granted amendment could never be demonstrated and
  the negotiation deadlocked at v1 with a proposal nobody could ever ratify. The
  boundary now returns a contract **per part**: the granting part gets its
  proposal, everyone else stays on the last ratified version until it lands.
- **Asking twice must not un-answer a verdict.** A part re-asks every generation
  until the contract moves, and the first implementation wrote `answered = false`
  on every ask — so the answering model was paid to make the same decision for
  ever, and each round minted another proposal (v2, v3, v4 …). An absent field now
  reads as unanswered and an answered one stays answered.
- **A request has to be read from every candidate, not from the winner.** The round
  in which a part most needs to ask is the round nobody passed, and that is exactly
  the round with no winner to read.
- **The gate's report says `accepted`, not `passed`.** `passed` at the top level is
  a COUNT of how many checks passed, so reading it as a boolean failed every
  composition with an empty list of reasons.
- **The orchestration was written twice** — once in the binary and once in the test
  meant to cover it — which is a test that exercises a re-spelling of the thing it
  is supposed to prove. It is one function now, `compose::run_parts`, and the
  binary is what a binary should be: it prints and it lands. The same fix the
  SurrealDB fixture needed when a second suite wanted it.

And two things about the FIXTURE that are really things about repair loops, worth
recording because they will bite a real run the same way:

- **A check's command reaches the model.** A repair prompt carries the failing
  check, so a gate that greps for the answer hands the answer over: the frontend
  passed in round one without asking anybody anything, because its check named the
  field it was supposed to negotiate for.
- **A part's own question comes back to it.** A repair lays the previous candidate
  into the prompt, so a model keyed on the words of its own request answers itself.
  The trigger had to be a marker only the *amended contract* carries.

## The failure modes I would watch for

- **Churn.** Full-dynamic amendment plus a model on both ends can renegotiate
  forever, each generation adjusting a field. `max_rounds` bounds it, but the
  symptom to look for is a run whose contract version climbs while no part's score
  does — that is a search spending its budget on paperwork, and it should be as
  loud as a failure.
- **Ownership disputes.** A denies, B insists, both are stubborn. The rule I would
  start with: **the part that must implement a surface owns its shape.** The
  backend owns response shapes; the frontend owns what it needs to render. Where
  that is genuinely ambiguous, it escalates to the human at the boundary — the
  intervention point ADR-0081 already argues for, with the request and the counter
  as its content.
- **A part that never finishes** starves the composition, and no amount of protocol
  fixes it. That is a completion problem (`stop-reason` exists) rather than a
  contract problem, but the composition step needs an answer for "one part has a
  winner and the other does not", and the honest one is: no PR, report which part
  failed and what it was still missing.

## What this does not do

- **No mid-generation communication**, deliberately. If a part genuinely cannot
  proceed for a whole generation, the fix is a smaller generation, not a mailbox.
- **No transport for the request**: it is a row and a model call, not a channel.
- **Nothing about more than two parts** beyond the obvious — three parts negotiate
  pairwise and the boundary resolution is the same, but the ownership rule gets
  harder and this ADR does not pretend otherwise.
- **No measurement.** Every claim here is a design argument. The first thing worth
  measuring is whether a decomposed run beats one branch given the whole job, and
  a control arm for that costs a run.
