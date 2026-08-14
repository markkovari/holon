# ADR-0081 — fitness, fuel, and what the swarm knows

*Three mechanisms a parallel agentic graph cannot run without: how a branch is
judged, how branches share what they learn, and how they are stopped.*

**Status: proposed.** Nothing here is built. It is written before the code
because all three decisions are expensive to reverse once agents depend on them,
and because two of the three have a version that looks obviously right and is
wrong.

Much of this is taken from reading alpha-swarm2 rather than from first
principles. Where it got something right, this says so and copies it. Where it
built a knob that does nothing, this says that too — because that failure mode is
the one worth designing against.

## Where this starts from

Already built and proven here:

| piece | what it gives this |
|---|---|
| environments (ADR-0078/0079) | a branch is a derived app with its own store and its own lineage |
| `knowledge:graph` (ADR-0080) | nodes, edges, traversal against SurrealDB |
| `comp:store/cas` (ADR-0065) | atomic compare-and-set — the only correct way to decrement a shared counter |
| `llm:inference` | `completion.usage` already reports prompt and completion tokens |
| `quota:meter` | period-based rate limiting, keyed by subject string |
| `lattice/src/lease.rs` | a lease whose TTL *is* the KV `max_age` — a dead holder frees its own slot |
| secrets + signing | a branch runs as an identity, and what it publishes is attributable |

Two things that look like they already solve this and do not:

- **`quota:meter` is not a budget.** It is a rate limit: subject, limit, period,
  reset. No parent granting to a child, no unspent allowance returning, no
  conservation. Used as fuel it gives every node an independent allowance, which
  is a licence to spend `nodes × limit`.
- **Nothing meters inference.** `completion.usage` returns token counts and no
  component reads them. The number this whole system needs is already arriving
  over the wire and being dropped on the floor.

## The lesson that shapes everything below

alpha-swarm2 has, in its shipped code:

- an agent and a worker tier, each with `time_limit_secs`, `token_limit` and
  `max_iterations` in config — **and no code path that reads them.** Only the
  orchestrator tier's fuel is enforced.
- `max_sub_plan_depth`, passed into `SwarmRunner::with_depth`, where `self.depth`
  is *assigned and never read*. Recursive sub-planning is not implemented.
- a `quality_passed` in the wave runner that is a stub evaluating to `true`, with
  a `// TODO` beside it. The real gate lives elsewhere; anything trusting that
  field is trusting a constant.
- a daily run cap that `continuous = true` bypasses entirely.

None of these are bugs of carelessness. They are what happens when a limit is
declared in configuration before anything enforces it: the config file reads like
a system with budgets, and the running system has one budget. This repo has been
bitten by the identical shape twice already — `comp:secrets/reader` shipped
unlinked, and `openai-provider` was never once called.

So the rule for this ADR, applied to every mechanism in it:

> **A limit that nothing enforces is documentation. Every limit here ships with a
> test that spends past it and watches it refuse — or it does not ship.**

---

## 1. Fitness: how a branch is judged

### The failure mode to design against

The tempting design is one scalar from an LLM judge: ask a model how good the
solution is, take the number, select on it. It fails specifically: **the loop's
stopping condition becomes non-reproducible.** Same graph, same inputs, two runs,
two different stopping points, and "why did it stop here" has no answer that
survives a re-run. Worse, selecting on a model's judgement is a closed loop — the
population evolves toward what the judge likes, which is not the goal.

### A static gate is not enough, and dynamic does not have to mean unreliable

`cargo check && cargo test` is a fine *floor* and a poor *gate*. It knows nothing
about the goal: a run whose goal was "add rate limiting" passes exactly the same
checks as one whose goal was "fix a typo". It cannot express "the endpoint answers
200 for this input" or "p99 stays under 50ms". And it cannot tighten as the graph
discovers what actually matters.

The reproducibility argument against a model-authored gate is still right, but it
was aimed at the wrong target. The property that matters is not *static*, it is:

> **A model may AUTHOR the gate. A model may never BE the gate.**

A gate can be generated per goal, as long as it is then **frozen, content-addressed
and re-runnable** — the same artefact judging every branch, and judging them the
same way on a re-run six weeks later. That is test-driven development with the
order preserved: write the acceptance criteria first, freeze them, then let the
swarm try to pass them. Dynamic per goal, static per run.

### Three tiers of check, and only one of them is static

1. **Invariants** — repo-wide, always on, never authored per run: it compiles,
   the pre-existing tests still pass, no secret was committed, lints did not
   regress. The do-no-harm floor. This is alpha-swarm2's whole gate, and as a
   floor it is exactly right.
2. **The goal spec** — authored once per run (by a person, or by a model and then
   frozen and hashed), naming what *this* goal means: a test that must exist and
   pass, a response that must have a shape, a benchmark that must not regress.
   This is the tier that answers "not dynamic enough".
3. **Discovered constraints** — checks added *mid-run* because a branch learned
   something the spec did not anticipate ("this deadlocks under concurrency").
   Genuinely dynamic, and the reason the gate is a living artefact.

Tier 3 needs one rule or it eats the other two:

> **The check set is append-only within a run. A gate may tighten; it may never
> loosen.**

A gate that can loosen lets the swarm win by lowering the bar, and it will,
because that is the cheapest available gradient. Append-only is mechanically
enforceable and needs no judgement.

### The gate and the fitness function are the same artefact

The draft of this ADR had them as two things and that was wrong. They are one
check list with a flag:

    record check {
        id: string,               // stable; the same check across generations
        required: bool,           // required => gate. desirable => fitness only.
        weight: u32,              // contribution to the score
        held-out: bool,           // see below
    }

- **accepted** = every `required` check passed.
- **score** = weighted fraction of *all* checks that passed, plus cheap
  deterministic measures (diff size, wall time, allocation count).

That collapses "how do I make the gate dynamic" and "where does fitness come
from" into one question — *what do we check, and which of those are
non-negotiable* — and it fixes a real problem the two-artefact design had:

**A binary gate gives no gradient at generation 1.** Nothing passes, every branch
scores identically "failed", and there is nothing to select on. That is the
sparse-reward problem and it is fatal to a search. Deriving the score from the
check *vector* rather than from the accepted *bool* gives a gradient from the
first generation — 3 of 11 checks passing beats 1 of 11 — and it is entirely
deterministic. No model is consulted for the primary selection signal.

The LLM keeps exactly two jobs, both of them optional and neither on the
critical path: **authoring** proposed checks (frozen before use), and the
**downgrade-only veto** from alpha-swarm2, which fails open.

### Held-out checks, because the agents can read the gate

An agent that can see the checks will write code that passes the checks rather
than code that solves the problem. That is Goodhart's law and it is not a risk,
it is a certainty given enough fuel.

So: **a subset of checks is held out.** The swarm sees the public set and
optimises against it; the held-out set runs only at promotion, on the candidate
about to be accepted. A branch that passes the public checks and fails the
held-out ones did not solve the problem — it fitted the gate, and that is a
distinct outcome worth recording, because it is evidence the public set is
gameable and needs a check adding.

This is a train/test split. The same reason applies and the same discipline: a
held-out check that has been used for selection is no longer held out.

### Completion: five endings, because the parent's next move differs

1. **`accepted`** — gate passed, veto did not fire. Stop; promote what it learned.
2. **`exhausted`** — fuel ran out. A good answer may be one step away, and the
   parent may refuel it. Recorded distinctly from failure, because a parent that
   cannot tell "wrong" from "ran out of money" abandons branches that were
   working.
3. **`plateaued`** — K consecutive generations with no score gain beyond ε. The
   honest end of most searches. K ≥ 2: one barren round is noise.
4. **`refuted`** — the branch proved its own approach cannot work. **After
   acceptance this is the most valuable outcome**, and the one a naive design
   discards; it stops every sibling re-walking the dead end. alpha-swarm2 keeps
   these in an `errors` namespace and injects them as "RECENT FAILED ATTEMPTS (do
   NOT repeat these approaches)".
5. **`abandoned`** — a human, or a parent reallocating.

The graph as a whole terminates when: the gate passes, or fuel is exhausted, or
every live branch has plateaued. All three are checkable without asking a model.

---

## 2. Knowledge: what the swarm shares

alpha-swarm2's design here is better than anything I would have proposed cold,
and most of this section is taken from it.

### The load-bearing idea: agents cannot write the trusted namespace

Five namespaces — `patterns`, `solutions`, `errors`, `trajectories`, `feedback` —
and the agent-facing tool permits writes to **only `solutions` and `errors`**.
`patterns` is written by the system, on a hook, and only after the quality gate
passed: the verified diff is distilled by a second, cheap LLM call into ≤900
characters and stored under a key that is the SHA-256 of the normalised goal.

That is the whole answer to knowledge poisoning, and it is worth naming as a
principle:

> **An agent may record what it observed. Only a passing gate may promote
> something to what the swarm believes.**

Raw model output never reaches the trusted pool. Everything in `patterns` is
downstream of a `cargo test` that actually passed.

### Scope: shared pool. The decision, and what it costs

**Decided: one shared pool per project**, matching alpha-swarm2. Copy-on-fork was
the first instinct here and it is wrong — it buys isolation nobody asked for and
loses the one property that makes a swarm better than one agent run N times.

But the cost is real and under-discussed, so it is written down rather than
discovered later:

**For a shared pool**

- Sibling 7 does not repeat sibling 3's mistake. This is the entire thesis.
- **Corroboration becomes possible.** Two branches independently finding the same
  thing is evidence; one branch finding it twice is not. Effectiveness weighting
  needs `n > 1` and a per-fork pool never gets there.
- One index to build and keep warm; each chunk is embedded once, not N times.
  Embedding is the largest recurring cost in this design.
- Dedup works: the same lesson from two branches reinforces one row.
- Knowledge outlives the run that produced it.

**Against a shared pool** — the honest list

- **Herding is the big one.** Every branch reads the same top-k and converges on
  the same approach. Parallel search whose branches all read the same advice is
  an expensive way to run one branch. *This is the failure mode that quietly
  destroys the value of the swarm, and it does not announce itself — the run
  looks healthy and the diversity is gone.*
- **Poisoning.** One branch writes a wrong lesson; everyone reads it. Mitigated by
  the promotion rule above, not eliminated.
- **Premature pruning.** A branch records "approach X fails" one step before it
  would have worked. Siblings now avoid X.
- **Confounded attribution.** Did branch 3 succeed because it was good, or because
  it read branch 1's pattern? Fitness comparison across branches gets muddier the
  more they share.
- Write contention on hot keys — real, and already solved by `comp:store/cas`.

**The mitigations, which are cheap and mostly missing from alpha-swarm2**

1. **Asymmetric visibility by trust.** Negative knowledge (`errors`, `refuted`)
   is visible to siblings *immediately*: its worst case is that you avoid
   something that would have worked, which costs a little diversity. Positive,
   prescriptive knowledge (`patterns`) is visible only *after a gate pass*, which
   is the anti-poisoning rule. The risk profiles are not symmetric so the
   visibility rules should not be either.
2. **A diversity budget.** Cap how much retrieved knowledge may enter any one
   branch's prompt — alpha-swarm2 already does this with a 1200-character budget,
   though for cost rather than for diversity. Deliberately vary retrieval across
   siblings: different `k`, different namespace mixes, and at least one branch
   that reads *nothing* and starts cold. A control arm is worth its cost: without
   one there is no way to tell whether the shared pool is helping.
3. **Provenance on every write** — which environment, which attempt, at what
   score. That is what keeps attribution recoverable when it matters.

### Embeddings come from a provider; chunking is ours

**Decided: no local models.** Embeddings come from a hosted provider behind the
`llm:inference` boundary already built, the same way completions do. That is a
deployment choice and it is the user's to make.

It does not, however, make the problem that argued for local models go away — it
sharpens it:

> **A hosted embedding model changes under you, and nothing errors.** Vectors
> stored last month come from a different space than vectors computed today.
> Cosine similarity between them is not an error; it is a number, and it is
> meaningless. The index rots silently and retrieval quality degrades in a way no
> alert fires for.

So the mitigation moves from *avoid the problem* to *detect and survive it*:

- **The model id and dimension are stored beside every vector**, not in config.
  An index holding two models is worse than no index, and this is what makes that
  detectable rather than invisible.
- **A model change is an index invalidation**, handled explicitly: the corpus is
  re-embedded, or the old vectors are quarantined. Never silently mixed.
- **A canary set** — a handful of fixed strings whose pairwise similarities are
  recorded at index build. If those drift, the model moved. This is cheap, it is
  the only way to notice a silent provider-side change, and it costs a few
  embeddings per run.
- **`min_similarity` thresholds are per-model**, because a threshold tuned on one
  model is a superstition on another.

**Chunking is a pure component and stays ours.** No model, no network, no host
capability — so nothing about the provider decision touches it, and it is the
half where the quality actually comes from:

- **Split on syntax, not token count.** A function, a struct, an `impl`, a doc
  section. Fixed windows cut declarations in half and produce chunks that
  retrieve well and read uselessly.
- **Prepend a context header** to every chunk — file path, parent symbol,
  language. A chunk reading only `fn open(...)` is unfindable; the same chunk
  headed `components/knowledge-graph/src/lib.rs · impl Conn · fn open` is
  findable three ways. This is worth more than the choice of embedding model.
- **Overlap for prose, none for code.** Code has natural boundaries; prose does
  not.
- **A chunk is a NODE in the knowledge graph**, edged to the entity it came from.
  This is the payoff for owning the chunker while having a graph: a retrieval hit
  expands to its structural neighbours by traversal — alpha-swarm2's third
  retrieval layer arriving for free rather than as separate machinery. A provider
  returns a flat list and can never do this.

Owning the chunker also caps the provider cost, which is the practical reason to
care: chunk boundaries decide how many embeddings a repository costs, and a
syntax-aware chunker produces far fewer, larger, more useful ones than a sliding
window.

And the half already built: **`search:index`** is a TF-IDF inverted index over
the KV store, pure WASI, no network, no provider. That is the sparse side of
hybrid retrieval, done — and it is the side that keeps working when the embedding
provider is down or has changed underneath you. What is missing is the dense side
and reciprocal-rank fusion over the two.

### Tiers and the router

Not every call deserves the same model. alpha-swarm2 has three tiers
(orchestrator, agent, worker) and picks between two of them with a **UCB1
contextual bandit** — arms are the tiers, context is a coarse shape of the goal
(`doc | simple | complex`), and the reward is attributed on **the real gate
verdict**, not on anything the model said about itself. That last detail is what
makes it a measurement rather than a preference, and it is worth copying exactly.

The router is a **decorator**, the same shape as the meter: exports
`llm:inference/inference`, imports it once per tier, and chooses. Which means the
composition is a chain, and the order is a decision:

    caller → router → meter → provider(tier)

The router picks first so that the meter charges what was actually spent at the
price of the tier that actually served it. Reversed, the meter would have to
guess a price before the model was chosen.

This is where tiers stop being a performance knob and become a *spending*
decision: a tier is a price, so routing is the main lever on what a run costs.
Three consequences:

- **The price list is per model, not per token.** `1000 prompt tokens on tier-A =
  N units, on tier-B = M units`, versioned with the run (see fuel, below).
- **Escalation is a purchase.** Retrying a failed cheap call on an expensive
  model is the correct move and it must come out of the same budget, or
  escalation becomes a way to spend money the budget cannot see.
- **The bandit's reward must be gate-attributed.** Reward a tier for what passed
  the gate, never for what a model claimed. Anything else selects for
  confidence.

Fallback matters as much as selection: when a tier is rate-limited or down, the
router degrades to another tier and **records that it did**. A run whose results
came from an unplanned fallback is not comparable to one that did not, and a
router that silently substitutes models poisons every fitness comparison
downstream.

### Confidence is derived from outcomes, never asserted

There is **no confidence field** on an entry. Instead a `pattern_effectiveness`
table records `(pattern_id, run_id, run_succeeded)` for every pattern that was
injected into a run, and retrieval reranks by it:

    similarity *= 0.5 + 0.5 * success_weight        // floor 0.5, neutral 0.75

A pattern that keeps being present when runs fail sinks. Nothing has to decide
how confident it is; the outcomes decide. **Copy this exactly.** A
self-reported confidence score is a number an agent optimises against.

### Dedup, decay, and travelling with the repo

- **Dedup** by UPSERT on `(namespace, project, key)`, key = hash of the
  normalised goal. Re-learning the same thing reinforces one row rather than
  growing the pool.
- **Skip duplicate work** entirely: `task_already_done` returns a past passing run
  above cosine 0.9 and the task is skipped.
- **Decay**: entries with `use_count < 2` last used over 30 days ago are deleted,
  plus a TTL sweep. Note the honest gap found while reading — *nothing schedules
  `decay`*; it is exposed but not driven by a loop. Another declared-but-not-run
  mechanism, and comp should schedule it or not claim it.
- **Export to the repo**: `.swarm/memory/patterns/<key>.md` and `errors/<key>.md`
  as markdown with frontmatter, plus `KNOWLEDGE.md`. Embeddings are never
  committed and are recomputed on import. This is a genuinely good idea — the
  knowledge is reviewable in a pull request, and a human can delete a pattern
  they disagree with.

### Retrieval is three layers, not one

1. **Semantic** — hybrid dense HNSW + BM25-lite, fused with reciprocal rank
   fusion; the reported similarity stays the dense cosine so a `min_similarity`
   threshold keeps meaning something. Positive guidance from
   `patterns`+`solutions`, negative from `errors`, with a character budget
   (1200) on what reaches the prompt.
2. **Co-edit statistics** — files historically changed together, min
   co-occurrence 2. Pure counting, no model.
3. **Code-graph traversal** — goal-named files → entities → 1 hop over
   `defines | implements | extends | imports` → structurally related files.

Only the third needs `knowledge:graph`. The first needs embeddings, which comp
does not have wired: `llm:inference/embed` exists and, like everything else in
that stack, has never been called. **That is the dependency to close first** —
without retrieval, a knowledge store is a write-only log.

---

## 2b. The human: no new interface, and the rate decides everything

The user authors the goal spec and reacts at intervention points. Jira was the
first candidate; on inspection it is the wrong first move, and so is building a
goal-and-notification system of our own. Both are answers to a question nobody
has measured yet.

### The number that decides this, and we do not have it

> **How many times does one run block on a human?**

If the answer is two or three, a command-line prompt is sufficient forever and
any UI built for it is waste. If the answer is fifty, no interface saves anyone —
a human is the bottleneck and the fix is *fewer questions*, not better
notifications.

Every argument for Jira, for a dashboard, for a notification pipeline, is really
an argument about that number. Committing to any of them now is deciding before
the number exists. The interruption rate is cheap to measure — record every
intervention as a node in the graph, which the design already does — and it
should be measured before anything is built for it.

### The two surfaces that already exist

**Git, for "before landing".** This is the highest-volume intervention and it
already has a mature interface: a branch and a pull request. The diff, the
discussion, the approval semantics, the audit trail, the mobile app, the
notifications — all of it already exists, is already in the workflow, and is
already read by the people who would be reading it. Approve is merge; reject is
close with a comment. alpha-swarm2 lands exactly this way (`swarm/auto` or
`swarm/issue-N`, plus a PR) and it is the part of its design that needs no
defending.

**The CLI, for "before spending".** This one happens once per run — the plan and
its estimated cost, at the only moment where stopping is free. `comp runs` to
list what is waiting, `holon approve <id>` to release it. There is already a
session, an auth path, and a tool people run.

Between them they cover the three interventions that earn a human's attention.
Neither needs anything built beyond a route and a command.

### The spec is a file in the repo

The gate section requires the goal spec to be **frozen and content-addressed**.
A file in the repository gets that for free, because git *is* content addressing:
the spec's identity is its blob hash, and the frozen-at-run-start requirement is
satisfied by recording a commit.

    .comp/goals/<name>.md

It is versioned, diffable, reviewable in the same PR as the work it governs, and
editable by the person who owns the goal without an API token. A ticket field is
none of those things, and reconciling a ticket edited mid-run against branches
already judged is a problem this simply does not have.

### So: a contract, with a trivial default

The loop needs two operations, and they are small enough that arguing about tools
is disproportionate to them:

    ask(question, options, context) -> pending-id
    resolve(pending-id) -> option<answer>

Everything else is an **adapter** behind that: the CLI and a PR to begin with;
Jira, Linear, Slack or a dashboard later, if the measured interruption rate ever
justifies one. `notify:dispatch` already exists for delivery — webhook, email or
SMS to a configured gateway, no vendor in the contract — so notification is a
component that is already written.

**The expensive mistake here is not choosing the wrong tool. It is coupling the
loop's state machine to somebody's issue schema before knowing what it needs to
ask.** A contract with a CLI behind it costs an afternoon and can be pointed at
Jira later. A Jira integration cannot be pointed back.

### How anyone finds out there is a question

The rule that decides this is not about transports:

> **The notification is never the question.** State is authoritative and durable;
> a notification is a lossy hint that state changed. A dropped hint costs
> latency; a dropped *question* loses a run.

Anything that inverts this — subscribing to a stream and treating the message as
the work item — turns every missed message into a branch waiting forever for an
answer nobody knows to give. This is the same failure as the earlier webhook
point, and it has the same fix: **list, then watch, then re-list on every hint,
and re-list on a timer regardless.** A client that has just started and a client
that has been connected for a week must reach the same answer.

**State lives in the platform.** `GET /api/interventions?pending` over the
control plane, which is already durable, already replicated, already the thing
the reconciler trusts, and already behind the session the CLI holds. This is
control-plane state, not swarm memory, and putting it anywhere else means a
second auth path and a second thing that can be stale.

Specifically **not SurrealDB**: live queries would need a websocket, the
knowledge-graph component speaks HTTP `/sql`, and a client subscribing to the
graph needs egress to the database plus credentials for it. That is the wrong
layer — the graph is what the swarm remembers, not what the platform is waiting
on.

**NATS is a nudge, and an optional one.** The lattice already carries
`comp.v1.<lattice>.…`, so a `…​.human.pending` subject is nearly free — but the
CLI is HTTP-only today, and a laptop is on the tailnet or it is not. So NATS can
never be the only path: it turns a two-second poll into an instant one for people
who can reach it, and nothing more.

### Three clients, in the order they are worth building

1. **`comp runs`** — one shot: what is waiting, oldest first. Works from
   anywhere the platform is reachable, needs no new dependency, and is the
   backstop every richer client falls back to.
2. **`comp watch`** — list, then poll (or subscribe when NATS is reachable), and
   print each question as it appears. This is most of a TUI's value for almost
   none of its code, and it composes: it is a thing you leave in a terminal
   split.
3. **A TUI** — panes, navigation, approve-in-place. Worth building only if the
   measured interruption rate says a person is answering often enough to want
   navigation. And by then it is a different *renderer* over the same two calls,
   not a different design.

Starting at 3 is the trap. A TUI written before the rate is known is a bet that
the rate is high — and if it is high, the right response was to reduce it.

**Away from the keyboard** is a separate problem with an existing answer:
`notify:dispatch` already sends a webhook, an email or an SMS through a
configured gateway with no vendor in the contract. "You are blocking run X" is
one call to a component that is already written. Push notification is not a
reason to build a UI.

### Suspension is not blocking, and it is not death

Whatever the surface, a human takes hours or days. That has one consequence worth
writing down because it would otherwise be found the hard way.

A node at an intervention point **checkpoints to the graph, releases its
environment, and resumes on the answer**. Blocking is wrong: an environment held
open across a weekend costs money to wait. Environments derive from a revision,
so re-deriving one is cheap and the checkpoint is what matters.

And where this meets the fuel design:

> **Fuel is refunded on lease expiry, so a crashed branch cannot strand it. A
> suspended branch looks exactly like a crashed one.**

Left alone, every human-gated branch has its fuel reclaimed while somebody is at
lunch, and resumes with nothing. Suspension must therefore be a distinct state
rather than an absence of heartbeat: a suspended node's fuel moves into **escrow
held by the parent** — earmarked, not redistributable — and returns on resume.
"Stopped answering" and "waiting for you" have to be different in the fuel
system, not only in a status field.

That is a sixth ending, alongside the five in the fitness section:

6. **`awaiting-human`** — checkpointed, environment released, fuel in escrow.
   Not running, not finished, not dead.

Escrow needs its own expiry, or a question nobody answers holds fuel forever: a
long timer — days, not minutes — that returns the fuel and reports the
abandonment. `sched:timer` already does durable timers.

### What earns an interrupt

A swarm that asks about everything is a swarm someone stops reading. Three:

- **Before spending** — the plan and its cost, once, at the start. The last
  moment where stopping is free.
- **Before landing** — a gate pass is necessary and not sufficient for a merge.
- **On a held-out failure** — a branch that passed the public checks and failed
  the held-out ones fitted the gate, which is evidence the spec is gameable and
  exactly what a person needs to see.

Everything else — a plateau, a refutation, an exhausted branch — is a *report*,
not a *question*. The test is whether the loop is blocked on the answer.

## 2c. Where the files live

A candidate change has to exist somewhere between the agent that wrote it and the
gate that judges it. The answer is forced rather than chosen:

> **Blob storage is authoritative. Disk is a materialisation, created to run a
> check and thrown away.**

Three reasons, and the first one alone decides it:

1. **A component has no filesystem.** Agents are components; that is the whole
   isolation model (ADR-0023). An agent *cannot* write to disk. The only place it
   can put a candidate is `blob:store`, so the authoritative copy is there whether
   or not that is convenient.
2. **`cargo` needs real files.** Nothing in wasm runs a compiler, so the check
   runner is native and materialises what it is given. That direction is
   one-way: disk is downstream of blobs, never the reverse.
3. **comp is a lattice.** A candidate on node 1's disk is invisible to a check
   runner on node 3. `blob:store` sits on `wasi:keyvalue`, which on NATS is
   JetStream KV — already replicated (ADR-0064, proven across three machines by
   killing one). Disk pins a candidate to a node; blobs let the swarm spread.

### The repository itself lives there — `vgit:store`

The first draft of this section kept a **warm clone on each node** and stored only
an overlay. That was a compromise dressed as a design: it puts the repository back
on a disk, which pins work to a node, needs `git` installed, and leaves the one
thing agents actually operate on in the one place they cannot reach.

The repository is in blob storage. All of it.

This is less exotic than it sounds, because **git is already a content-addressed
object store with a tree over it**: a blob is bytes, a tree is a listing, a commit
points at a tree and its parents, and each is named by the SHA-1 of its own
serialisation. `blob:store` is a content-addressed object store. The mapping is
the identity function, not an emulation.

    objects → blob:store      immutable, named by content, so a plain put is safe
    refs    → comp:store/cas  the only mutable thing in git, so the only guarded write

That split is the clearest illustration in the repo of why both primitives exist.

What it buys:

- **A branch costs one ref.** Forking an environment is a small write rather than
  a copy, and twenty branches share every object none of them changed.
- **A candidate is a commit id.** Forty characters, meaningful on any node,
  comparable by equality. Identity stops being a judgement call.
- **A change costs its depth, not the repository's size.** Writing a file rewrites
  the trees along its path and reuses every sibling subtree by id.
- **Agents can do git at all.** They have no filesystem; this they can reach.

**The object ids are real git ids** — SHA-1 over `<type> <len>\0<payload>`, byte
for byte what `git hash-object` produces. That is checked against the actual `git`
binary rather than against our own expectations, because "we hash consistently"
and "we hash the way git does" are different claims and only the second is worth
anything. It makes provenance verifiable end to end: an id here is the id
everywhere.

The trap that check caught the value of is tree ordering: entries sort by name
*except* that a subtree sorts as if its name ended in `/`, so `foo` the directory
comes after `foo.txt` while `foo` the file comes before it. Get it wrong and the
tree still serialises, still hashes, and hashes to something git disagrees with —
silently. Same for the subtree mode, which git writes as `40000` and everyone
writes as `040000`.

Deliberately **not** built: packfiles, delta compression, smart-HTTP, merges. None
are needed, because `git:forge` submits content over a hosting API and nothing
here ever speaks git over the wire. Building them because "a git implementation
should have them" would be building a second git.

Materialising to disk is still required for the gate — `cargo` needs real files —
but it is now "write out this tree", with no clone and no `git` binary in the
path.

This is the same shape the forge already takes: `base_tree` plus changed blobs.
So one representation runs the length of the pipeline —

    agent writes overlay → gate materialises base+overlay → forge submits base+overlay

— with no translation step anywhere, which is why `git:forge/repo` takes whole
files rather than a patch. A patch has to apply to something; an overlay is
already the answer.

### What content-addressing buys

Name each blob by the hash of its bytes and two things follow for free:

- **Dedup, and it is a fuel saving rather than a tidiness one.** Two branches that
  independently produce the identical change are the *same candidate*. Gate it
  once. In a swarm exploring near each other, that collision is common.
- **A candidate's identity is its hash.** Comparing two candidates is comparing
  two strings, and "did generation 4 actually differ from generation 3" stops
  being a judgement call.

It is also git's own model, which is why the last step maps across without
thinking about it.

### The container is owned by the run, not by the branch

One wrinkle worth naming, because the isolation model creates it. Every app gets
its own store, named after it, and an environment is a derived app — so a
candidate written into an environment's own container is a candidate its parent
cannot read. The selector would have nothing to select from.

So candidates go in **the run's container**, owned by the orchestrating app, which
each branch is granted access to. The branch's isolation is still real — it cannot
reach another *run* — but siblings share the surface their parent has to compare
them on, deliberately.

### Retention

Overlays accumulate: branches × generations × changed files, most of them
discarded within minutes. They are scoped to the run and deleted when it ends,
with the winner's overlay outliving it only as far as the pull request, which is
where it becomes git's problem instead. Nothing here is durable storage and
nothing should treat it as such.

## 3. Fuel: budgeting with propagation

alpha-swarm2 has fuel in three dimensions — time, tokens, iterations — checked in
that order at the top of the retry loop, with exponential backoff between
attempts. It has **no parent→child propagation** (tokens aggregate *upward*,
never decrement downward), **no per-node budget**, and **no monetary cost at all**
— zero occurrences of price or cost across the codebase. And, as noted, it
enforces only the orchestrator tier.

So propagation is the part comp has to design rather than copy.

### The invariant everything follows from

> **Fuel is conserved. A node cannot mint it. Σ(live balances) + spent + refunded
> = the original grant, at every instant.**

Stated first because it is the only property here that can be *tested*, and
because every plausible budget design lacking it leaks. Without conservation,
"this run has a budget of N" is a hope; with it, it is arithmetic, and a test can
assert it after an arbitrary interleaving of spawns, spends, crashes and refunds.

### Mechanics

- A run begins with one grant, held by the root.
- **Spawning transfers.** A parent moves fuel from its own balance into the
  child's. It cannot create fuel; with none, it cannot spawn. Depth becomes
  self-limiting — though a depth cap exists anyway, because splitting into
  thousands of one-unit children is conservative and useless. (And a depth cap
  that is assigned and never read is alpha-swarm2's bug, so it gets a test.)
- **Spend reserves first, settles after.** Reserve the estimate, do the work,
  settle the actual, return the difference. `quota:meter` already has this shape;
  it is the *hierarchy* that is missing, not the operation.
- **Death refunds.** Any of the five endings returns the remaining balance to the
  parent. A crashed child must refund too, or a run leaks fuel every time
  something fails — exactly when it can least afford it. This is what
  `lattice/src/lease.rs` is for: a balance held under a lease whose expiry *is*
  the refund, so a dead branch cannot strand fuel.
- **Every mutation is a CAS.** Two children settling against one parent
  concurrently is the default case. This is what ADR-0065 was for.

### How much to give a child

- **Equal split** — at branching `b`, depth `d`, a leaf holds `grant / b^d`. With
  `b=3, d=5` that is 0.4% each: every leaf starves before reaching the depth
  where the answer is.
- **Proportional to fitness** — all fuel follows the current best branch. Pure
  exploitation; the search never learns that the runner-up was second only
  because it was unfunded.
- **Floor plus proportional (the choice).** Every child gets a floor — enough for
  one honest attempt and an evaluation, or it should not have been spawned — and
  the remainder is split by expected value. The floor is exploration, the
  remainder is exploitation, and the trade-off is explicit instead of emergent.

A parent also **keeps a reserve**, because refuelling a plateaued-but-close child
is the highest-value thing it does, and a parent that distributed everything
cannot.

### The unit, and the thing alpha-swarm2 does not have

Three separate limits (time, tokens, iterations) cannot be traded against each
other. A branch that is one cheap test-run from an answer cannot spend leftover
tokens on it.

So: **one abstract unit, with a price list in config.** `1000 prompt tokens = N
units`, `1 CPU-second = M units`. The price list is versioned with the run. Then a
run costs one number, comparable across generations, with the exchange rates
visible rather than implied — and a monetary figure is one more multiplication
away, which is the thing alpha-swarm2 cannot do at all.

Iteration count stays as a separate hard cap. It is not a resource, it is a
loop-guard.

### Enforcement at the chokepoint, not by good behaviour

A budget agents must choose to check is not a budget. Enforcement goes where the
spending physically happens.

The mechanism is a **metered decorator**: a component that *exports*
`llm:inference/inference` and *imports* `llm:inference/inference`, sitting between
caller and provider. The caller cannot tell, and cannot bypass it — its import is
wired by composition, and a component cannot dial what its manifest does not
allow.

Verified before proposing: WIT accepts a world importing and exporting the same
interface, and `wasm-tools` resolves it. Whether `wac plug` wires it without
self-satisfying the import is the one thing to confirm at build time.

That decorator is the only place that needs to reserve, settle and refund — and
the only place that has ever read `completion.usage`.

### When fuel runs out

The node stops, keeps its best result, records `exhausted`, refunds nothing (it
has nothing). It does **not** fail. A parent must distinguish "this approach is
wrong" from "this ran out of money"; conflating them abandons the most expensive
and often most promising lines of work.

---

## What has to be tested before any of this is believed

Not a wish-list. This is the ADR's own rule applied to itself.

- **Conservation under concurrency** — spawn a tree, spend randomly, kill nodes
  at random, assert `Σ balances + spent + refunded == grant`.
- **A crashed child refunds** — kill it without a clean exit; the lease expires
  and the parent gets its fuel back.
- **A child cannot overspend** — racing a sibling settling against the same
  parent.
- **The decorator cannot be bypassed** — a component that tries to reach a
  provider directly is refused by egress, and the test asserts the refusal.
- **Every declared limit refuses something** — one test per limit that spends past
  it. This is the rule from the top of this document, and it is what separates
  this from a configuration file that describes a system nobody built.
- **A gate stub cannot pass for a gate** — assert the gate actually failed
  something, so a `-> true` can never sit behind it unnoticed.
- **Decay runs** — or is not claimed.

## Open questions this does not answer

- **How is herding actually detected?** The shared pool is decided and its
  worst failure mode is a silent collapse of diversity. Measuring it — pairwise
  distance between sibling diffs, or how much of each branch's prompt came from
  retrieval — is unsolved here, and a swarm that cannot see its own diversity
  cannot notice it losing it.
- **What are the tiers, concretely?** The router is designed and the tier list is
  not. It needs real models with real prices, and a measurement of which classes
  of work the cheap tier actually completes — which cannot be guessed and has to
  be run.
- **Re-embedding cost on a provider model change.** The canary detects the drift;
  nothing here budgets for the re-index it triggers, which for a large corpus is
  the single largest bill this system can generate.
- **Is `comp watch` polling or subscribing?** Both, ideally, but the CLI has no
  NATS client and adding one assumes the operator is on the tailnet. Polling
  works everywhere and is the honest default; the subscription is an optimisation
  that must never become a requirement.
- **What IS the interruption rate?** The whole human-interface question reduces
  to it and it is unmeasured. Nothing should be built for a human until a run has
  been watched and counted.
- **Does an answer survive a comp restart?** The checkpoint is in the graph, but
  nothing here says what happens to an approval that arrives while the platform
  is down. `webhook:ingest` dedups replays; it does not replay what was missed.
  A CLI-first surface dodges this — a poll finds the answer whenever it comes
  back up — which is one more reason to start there.
- **Who runs the loop?** This ADR describes the mechanisms, not the driver. A
  component that spawns, evaluates, selects and refuels is a separate decision,
  and ADR-0079 only established that a component *can* fork its own app.
