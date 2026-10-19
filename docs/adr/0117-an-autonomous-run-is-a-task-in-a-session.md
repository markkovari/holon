# ADR-0117 — An autonomous run is a task in a session

*Two programs run an agent and each owns a loop. The work since the second one was written — streaming,
a queue, steering, an approval that outlives the process — went into the one the personal stack does not
run on, and the one it does run on is built by no workflow.*

**Status: proposed.** Nothing here is built; the first step of it, one wire-format crate, is
[ADR-0116](0116-one-crate-writes-and-reads-the-model-wire-formats.md), which said in its last paragraph
that merging the two programs "is a separate decision with a larger blast radius". This is that
decision, written before any of it so that the order can be argued with while it is cheap.

## What exists, read from the code

| | [`agent-runtime`](../../agent-runtime/) | [`comp-agentd`](../../reconciler/daemons/agentd/) |
|---|---|---|
| what it is | autonomous agents: a spec, triggers, memory, run unattended | a session API: a caller holds a conversation with one agent |
| built on | threads, blocking `reqwest`, `tiny_http` | `tokio`, `axum`, `tonic` |
| how a model asks for a tool | a JSON object in plain text, read loosely (`agent.rs` `json_calls`, `loose_calls`), so any model can: Apple's on-device one, small `mlx` ones | the provider's native tool calling, in both dialects |
| what a tool is | memory, a shared store, events, tasks, files, `http_get`, connectors, calendar, food photos, speech, and the ones that manage other agents — granted per agent | `list_dir`, `read_file`, `write_file`, `run`, in the session's workspace |
| what wakes a run | cron, an event bus with per-consumer offsets, a store change, HTTP, one-shot timers | nothing: a caller sends a task |
| approvals | an in-memory wait (`runtime.rs` `approve`), five minutes by default, then a denial; a restart drops it | per call, parked in `comp-park` with `--park-url` ([0113](0113-a-task-waiting-for-a-person-is-a-parked-call.md)), with a reach of one call, the task or the session ([0114](0114-an-approval-can-reach-beyond-one-call.md)) |
| what it spends | tokens per run, per day and per chain of wake-ups | integer micro-USD per session, from `comp-fuel`'s price table |
| what it leaves | a `RunRecord` per run, W3C trace context, OTLP export | one ordered event stream per session, a snapshot and a tail ([0107](0107-a-client-catches-up-from-a-snapshot-then-tails.md)) |
| what it streams | nothing: a run waits for the whole reply | `text_delta`, as SSE and gRPC |
| which model | a spec's model, or a fallback chain (a hosted one first, a local one as the floor), with local servers started on demand | one per session |
| who runs it | `io.holon.runtime`, a launchd service: the personal stack (`infra/launchd/install.sh`), with `agent-matrix` in front of it, or `gpui-console` embedding it | nobody yet: `apps/park.toml` points `comp-resume` at it, and CI builds its image and runs `--help` |
| who checks it | no workflow builds, lints or tests `agent-runtime`, `agent-matrix` or `gpui-console` | the whole `reconciler/` workspace's CI |

Of the three that no workflow looks at, `gpui-console` does not compile today (`ModelSpec` has fields it
was last built without) and `agent-runtime` is not rustfmt-clean, and nothing is red, because nothing
looks.

What is the same is the middle of the loop: ask the model, run the tools it asked for after a human has
said yes, hand the results back, until it answers or runs out of steps or money. That is written twice,
and so are the things around it: the model call (one crate since [0116](0116-one-crate-writes-and-reads-the-model-wire-formats.md)),
the approval, the spend limit and the confinement of a file tool to a directory. Every improvement since
[0102](0102-an-agent-session-is-a-contract-first.md) went into `comp-agentd` and is not available to an
agent that runs unattended — which is where an approval that outlives the process matters most, because
nobody is there to answer inside five minutes.

## The decision

**A run is a task in a session, and the loop is the daemon's.** `agent-runtime` keeps everything that
decides *whether, when and with what* an agent runs — triggers, the wake graph and its guards (depth,
cycles, rate, the circuit breaker, the chain's shared budget), the store, memory, projects as grants,
connectors — and hands "run this to its end" to `comp-agentd`. One loop to fix, to measure and to park.

**The runtime's tools stay in the runtime.** A session declares tools; a call to one is *requested* of
the client in the event stream, and the client answers it. Between the two the task is parked the way an
approval is ([0113](0113-a-task-waiting-for-a-person-is-a-parked-call.md): a checkpoint, a ticket, a
delivery that resumes), so a wait on a client — a calendar call, another agent's whole run — survives a
restart for the same reason an approval does. The daemon's four workspace tools remain what a session
gets when it declares none.

**A model with no native tool calling is served by the same loop.** The JSON-in-text protocol becomes a
mode of the session, and its readers move into `holon-llm` with their tests, because they are the one
part of `agent-runtime`'s loop that exists because of what models do and not because of what
`agent-runtime` is. This is useful to the daemon before anything else moves: [0102](0102-an-agent-session-is-a-contract-first.md)
recorded that a small model behind `mlx` "can look like it never calls tools", and `agent-runtime` has
been reading what those models actually write for as long as it has existed.

**What a run is told moves to where a session is made.** A system prompt per session, a step limit per
session (a daemon-wide flag today), the spend limit, and which tools ask first. A run does not share a
conversation with the one before it — what an agent knows it keeps with `remember` — so a run is a
session of its own, closed with it, and the daemon needs sessions that cost nothing to make and nothing
to keep before the runtime can use it that way.

**The order puts a net under the live stack first, and nothing changes for it until it asks to.**

| | step | what is true after |
|---|---|---|
| 0 | `agent-runtime` and `agent-matrix` are built, linted and tested in CI (as packages of `reconciler/` per [0115](0115-the-reconciler-is-a-workspace-of-single-purpose-packages.md), or as a job where they are, whichever is the smaller first PR) | the programs the personal stack runs on are checked; nothing about them changed |
| 1 | the text tool protocol into `holon-llm`, used by `agent-runtime`; then by `comp-agentd` as `--tool-protocol` per session | one reader of what small models write; the daemon serves them |
| 2 | `comp-agentd`: a system prompt and limits per session, declared tools, calls requested of the client and parked | a client that is not a person can supply the tools |
| 3 | `agent-runtime`: `engine` on an agent's spec, `legacy` by default, `agentd` per agent; the runtime builds its `RunRecord` and spans from the session's events | one agent at a time runs on the daemon, and the other loop is still there to go back to |
| 4 | the default flips once every agent in the state directory has run on the daemon; their approvals are its parked ones | the five-minute wait is gone |
| 5 | the legacy loop and its approval wait are deleted | one loop |

Each step is its own PR and keeps the tests of what it moves. Steps 0 and 1 stand without the rest: they
are worth doing if 2 to 5 are never taken.

## What this does not decide

- **Whether they end as one process.** A daemon that serves a contract and a service that wakes agents
  have different lifecycles; after step 5 the runtime is a client of the daemon and may stay a separate
  program for good.
- **How the console embeds it.** `gpui-console` links `agent-runtime` in-process. It stays on the legacy
  engine until step 4 and then either starts the daemon or links its library; and it is outside CI by
  choice (GPUI needs macOS), which step 0 does not change.
- **The names.** Which of these is called `holon-` anything is the rename's decision, not this one's.

## Alternatives

**Share only the leaves.** What 0116 did for the wire format, and the text protocol in step 1 is one
more. Cheap, each one removes real duplication, and they are all done first. But the loop, the approval,
the spend and the stream stay two, and each daemon feature lands once. Taken as far as it goes, this is
steps 0 and 1.

**One loop crate that both embed.** Cleanest on paper: a `Host` trait (`agent-runtime` already has
one) and a loop over it. The two loops differ in what a crate has to be to hold both — `agent-runtime`
is threads and blocking HTTP, and the daemon is `tokio` with parked tasks — so the shared crate is async
and about eleven thousand lines of the live stack are rewritten onto it with no CI. Steps 0
to 3 reach the same end without touching the runtime's core: it stays blocking and speaks to the daemon.

**Retire `comp-agentd`.** Serve `agent.proto` from `agent-runtime`. Loses streaming, the queue,
steering and parked approvals, all built and tested, and gives the runtime `tokio`, `tonic` and `axum`
to carry.

**Retire `agent-runtime`.** Loses what makes it an assistant: triggers, memory, projects, connectors,
the API `agent-matrix` is written against.

**Leave both.** The cheapest today. The personal stack stays the one part of the repository that nothing
checks, and every fix to a loop is made in two places or in one.

## What it costs

- **A hop.** A run goes through loopback HTTP and an event stream instead of a function call. Against
  the seconds a model takes this is small, and it has not been measured, which step 3 does first.
- **A dependency on NATS for the durable part.** Parking needs `comp-park`; without `--park-url` the
  daemon's approval is a held future, as the runtime's is, so a machine without NATS loses nothing it
  has, and does not gain the reach.
- **Two views of a run for as long as both engines exist.** `RunRecord` and the traces are the runtime's
  and are built from the session's events; the daemon's stream is not changed to look like them.
