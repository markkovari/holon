# ADR-0107 — A client catches up from a snapshot, then tails

*A page reload, a bot restart and a second watcher all arrive knowing nothing. The
log has everything, and reading all of it is the wrong way to find out.*

**Status: accepted, and built.** `GET /v1/sessions/{id}/state` in
[`api/openapi.yaml`](../../api/openapi.yaml) and `GetSessionState` in
[`api/holon/v1/agent.proto`](../../api/holon/v1/agent.proto), served by `comp-agentd`;
the fold is [`reconciler/src/bin/agentd/state.rs`](../../reconciler/daemons/agentd/src/state.rs).
Extends [ADR-0102](0102-an-agent-session-is-a-contract-first.md); additive, so no client
of that contract changes.

## The problem

ADR-0102 made the event stream resumable: every event has a gapless per-session `seq`, and
a client that drops reconnects with `after_seq` and loses nothing. That serves a client
that was watching. A client that was *not* — a page that has just loaded, a bot that has
just restarted, a second operator opening the same session — has no cursor, so it replays
from 0. A session's log is dominated by `text_delta`: one event per chunk of the model's
words. A long session is tens of thousands of events to read in order to learn "what has
been said, and is anything waiting on me".

And each such client has to turn those events back into a conversation, in its own
language. That fold is the same in every one of them, and it is where they would drift:
which events make a message, when a message is finished, which calls are still waiting.

## What was read

[pi-durable](https://github.com/earendil-works/pi/tree/main/packages/durable) offers
`viewState()`, described in one write-up as "a subscribable view with committed entries,
running tool output, and queued inputs, which helps reconnecting UI clients". That is the
need, named. What was read is the write-up, not the source.

## The decision

**One read that returns the state and the cursor together.** `getSessionState` returns

- `session` — what `getSession` returns;
- `seq` — the last event the rest reflects (0 for a session with none);
- `transcript` — the conversation as a client draws it, oldest first: `user_message`,
  `assistant_message`, `tool_call`, `task_ended`, each carrying the `seq` of the event that
  began it;
- `pending_approvals` — calls waiting on `submitApproval`, which are what a bot has to ask a
  person about;
- `pending_steers` — see [ADR-0108](0108-a-running-task-takes-input-at-a-boundary.md).
- `queued_tasks` — tasks accepted onto a busy session and not yet started, in the order they
  will start; see [ADR-0112](0112-a-busy-session-can-queue-a-task.md).

A client reads it once, then opens the stream with `after_seq = seq`. Because the state and
`seq` are computed under one lock over one log, that stream delivers exactly the events
that came after: no gap, nothing twice. It is a cursor, not a promise about time — a
snapshot is as stale as the round trip.

**The state is a fold of the events, and nothing else.** The log is already the one truth
(`session.rs`: spend and task states are rebuilt from it on load). A snapshot that kept its
own copy would be a second truth to keep in step, and the cursor could then disagree with
it. So `state::fold` is a pure function of `&[Event]`, and the state of a session after a
restart is the same fold over the same log: tested, not assumed.

**It does not read the conversation the model sees.** `Session::history` is a
`tokio::Mutex` held for the whole length of a task, so a snapshot of a running session
could not take it without waiting for the task to end; and it holds the provider's
messages, which is not what a client shows. The fold also gets something the history does
not have: the message the model is *still writing*, marked `in_progress`, from the deltas
so far.

**The fold's `match` over `Payload` has no wildcard.** A new event kind is a compile error
until someone decides where it appears in the state, which is the point: an event a
snapshot client cannot see is one a reconnecting client silently misses.

## What this does not do

- **Cost is the log.** A call folds every event the session has, and the log is held in
  memory in full (ADR-0102 names this). Fine for one operator's daemon; an incremental view
  updated per event is the change if a session reaches the point where a snapshot is
  noticeable, and the fold's tests are the spec for it.
- **No pagination** of the transcript, and **no streaming of a running tool's output**:
  the daemon does not have any. A tool's output appears when the call finishes.
- **Not atomic with the stream.** Snapshot and tail are two calls joined by a cursor. A
  single call that returns the state and then keeps streaming was considered and rejected:
  it needs a framing of "this frame is the snapshot" on both SSE and gRPC, to save a round
  trip a client makes once.
- **Connect's own protocol** is still not served (ADR-0102).

## Proof

[`state.rs`](../../reconciler/daemons/agentd/src/state.rs) folds synthetic logs: deltas of one
message join and only the last is in progress, a call waiting on a person is pending until
answered, and a task that ends — a cancel mid-approval — abandons its waiting call and its
unapplied steer so a bot does not ask about a ghost. Through a real router
([`agentd/tests.rs`](../../reconciler/daemons/agentd/src/tests.rs)): the state mid-task
(waiting on an approval) and at the end, with `seq` equal to the number of events the
stream delivered; the stream opened at that cursor starts at `seq + 1` with the next
task's `task_started`; the same state, at the same cursor, from a second daemon over the
first's `--state-dir`; and the same state over gRPC. Run by hand against the real
binary with `--provider mock`.

## Alternatives

- **Have every client fold the log.** The status quo. Rejected: the fold is the same
  everywhere and is where clients drift, and it costs the whole log to reach it.
- **Snapshot from `Session::history`.** Rejected above: held for the length of a task, and
  the wrong shape.
- **Keep a materialised view updated on every event.** Not now: a second copy of the truth
  to keep in step, for a saving nobody has needed yet.
