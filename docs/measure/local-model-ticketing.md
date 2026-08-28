# A goal executed by a model on one desk

What happened when the loop was pointed at a 27B running on an M2 Pro instead of at
an API, measured rather than estimated. The app is `events:ticketing` — free tickets
for events with a hard capacity, QR codes, check-in and swaps — and the goal is one
part of it: `src/events.rs`, five routes behind a real authorizer.

## The setup

| | |
|---|---|
| box | csatapaci, M2 Pro (T6020), 96 GB unified, ~200 GB/s |
| model | `mlx-community/Qwen3.8-27B-4bit` — 27B **dense**, not a MoE |
| serving | `mlx_lm.server`, `--prompt-cache-size 4` |
| path | `comp-goalrun` → `openai-shim` (:8788) → mlx (:8080) |

The shim is a translator, not an alternative: the loop speaks Anthropic's
`/v1/messages`, mlx speaks OpenAI's `/v1/chat/completions`.

## What had to be fixed before a single token was spent

**The loop could not start at all, and had not been able to for two merges.**
`graph:fitness/evaluator` went to `@0.2.0` in #147 and `graph:run/driver` in #148;
thirteen `links:` entries across seven fixtures still asked for `@0.1.0`. `wac plug`
matches an import to an export on the whole versioned string, so every one of those
links resolved to nothing. The symptom:

    goalrun.acme.test never served within 180s — last: HTTP 503 Service Unavailable

which names no interface, no component and no version. `fixtures.rs` passed
throughout and was right to: it asks whether a fixture parses and whether its ids
resolve against each other. Nothing compared a version in a YAML file to a version
in a built artifact. `reconciler/tests/fixtureversions.rs` now does — 69 links
across 40 fixtures.

**The model the config named was not on the machine.** `.comp/csatapaci.env`
documented `Qwen3-Coder-30B-A3B-Instruct-4bit` with benchmarks, a context window and
a prompt-cache incident attached to it. `~/mlx-models` held Qwen2.5-Coder 14B and
32B, and the mlx logs stopped on June 6. That is also why the server was down: it
cannot load a model that is not on disk.

## Thinking mode is the whole cost

Qwen3.8 thinks by default at `xhigh`. Same small task, twice:

| | tokens | wall | finished |
|---|---|---|---|
| thinking on | 1200+ (hit the cap) | 106 s | no |
| thinking off | 158 | **13 s** | yes |

Decode is ~11 tok/s either way — 10.4–11.3 tok/s measured, 1200 tokens in 106–115 s.
Eight times the tokens for one answer. `OPENAI_EXTRA_JSON` in the shim carries
`{"chat_template_kwargs":{"enable_thinking":false}}` and the model card's
non-thinking sampling parameters.

## Run 1: two branches, 17 minutes, score 0

| | |
|---|---|
| wall clock | 17 min 17 s |
| model calls | 4 |
| per call | 367 s, 473 s, 479 s, and one 529 after 540 s |
| gate calls | 6.6 s, 6.8 s, 8.9 s |
| tokens | 23,322 on the branch that ran |
| branch-0 | `provider-down` — 0 attempts, 0 tokens |
| branch-1 | 2 attempts, score 0 |

**Two branches is one too many.** `branch-0` died to a 529 because two concurrent
six-minute generations is more than this server will hold. On an API where a call is
seconds, four branches buy diversity; here they buy a dead branch. The batching
measurement that justified concurrency belongs to a 3B-active MoE, and this is a
dense 27B.

**A gate call is ~7 seconds against a model call of ~7 minutes.** The gate is free.
Whatever else is true, there is no reason to judge less.

## And the score of 0 was the harness's fault

The failing check:

    FAILED: ?state=open did not list the open event just created —
    find_by wants the JSON ENCODING of the value:
    {"events":[{"capacity":1,…,"state":"open","title":"The Last Seat In The House"},
               {"capacity":3,…,"state":"open","title":"Rust, Wasm and a Free Drink"},
               {"capacity":50,…,"state":"open","title":"Wasm Night"}]}

Every open event came back, including the one the gate had just created. The filter
worked. The gate was grepping the body for the new event's `id`, and no entry had one
— because CONTRACT.md's list row never said to include one. Only the single-event row
did.

So the model implemented exactly what it was told, and the gate failed it for
something the contract did not require.

The second half is worse than the first. The failure message **asserted a cause**:
*"find_by wants the JSON ENCODING of the value"*. That was a guess, it was wrong, and
ADR-0088 says a gate's output IS the next prompt — so the repair round was sent to
fix a query that already worked. This is the same shape as the bug `gate-lib.sh`
already records, where three generations of a real model were judged against a
quoting error.

**A gate may report what it observed. It must not invent why.** The rewritten check
makes two claims with two messages: one for "this event is missing while others are
present", which is the filter, and one for "no entry has an id", which is the shape.

## Run 2, after the gate was fixed: a different failure, and a sharper one

Same model, same prompt size, opposite behaviour. From the shim's log:

    run 1:  472848ms  in=9142tok  out=1908tok  6228B   <- real answers, with code
            479446ms  in=9153tok  out=1979tok  6522B
            367225ms  in=11262tok out= 928tok  3626B
            FAILED after 540021ms                      <- the 529

    run 2:   92295ms  in=9170tok  out=  49tok    83B   <- gave up
              9478ms  in=9231tok  out= 101tok    83B   <- prompt-cache hit
              4823ms  in=9170tok  out=  49tok    83B

Run 1 generated 1,908- and 1,979-token answers containing code. Run 2 produced
49-101 tokens, six times, always exactly 83 bytes of text:

    I'll read the contract and existing files to understand what I need to implement.

The prompt cache was the obvious suspect and it is not the cause: after a full
server restart with `--prompt-cache-size 1` the same six calls came back identical,
now with a full 95-106 s prefill. It is deterministic.

**What the model does when it is not given something is ASK FOR IT**, and it asks in
a format nobody defined. Measured directly against the same server:

| prompt | out | wall | emitted blocks? |
|---|---|---|---|
| 305 tokens, goal only | 19 | 3.5 s | no — invented `=== READ: CONTRACT.md` |
| 8,784 tokens, contract + lib.rs + WITs | 2,734 | 336 s | **yes**, a correct `=== FILE:` block |

So the model can do the task and does it when the files are in front of it. That is
the opposite of the expected long-context failure: it is not that a large prompt
buried the format instruction, it is that a model trained to use tools reaches for
one.

`agent-writer`'s system prompt already carries a doc comment about this exact
symptom with Qwen3-Coder-30B — an answer discarded as "no edit or file blocks" that
was a FORMAT failure "wearing the costume of a model that cannot code". It is the
same costume on a different model, and the fix that worked for the first one (show
an example, name the markers, forbid fences) does not address this one. What this
model needs said is that it CANNOT read files, and that everything it will be given
is already below.

That is the open question this run leaves: the direct test and the loop send
near-identical content and get opposite answers, so the difference is in how
`agent-writer` frames it, not in the model or the context.

## What this says about making goal execution better

1. **The scaffolding is still the bottleneck, exactly as `complex-five.md` found.**
   Two of the three failures here were the harness — a version drift that stopped the
   fleet, and a gate that asserted a cause. Neither was the model. The one thing the
   model was actually asked to do, it did.
2. **A gate must not diagnose.** Observation and cause are different claims and only
   one of them is cheap to get right. Where a cause is worth suggesting, suggest it
   as a conditional next to the evidence, not as the finding.
3. **Concurrency is a property of the server, not of the loop.** `BRANCHES` should
   come from what the model host will hold. Here that is 1.
4. **Certify before spending.** `goal-rehearse` and `--smoke` between them caught an
   untracked gate script — *"an UNTRACKED file is not in the base tree… Nothing was
   spent"* — and confirmed every check fails on the base tree for a judgeable reason
   rather than a compile error. Both cost zero tokens.
5. **The version guards need to come in pairs.** #147 added a check that a shape
   cannot move without its version. It did not add a check that the version moving
   takes its consumers with it, and the second failure is louder and harder to read
   than the first.
