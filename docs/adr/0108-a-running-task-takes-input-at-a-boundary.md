# ADR-0108 — A running task takes input at a boundary

*One task at a time is right, and it leaves a person who wants to redirect the agent
mid-task with two choices: wait, or throw the work away.*

**Status: accepted, and built.** `POST /v1/sessions/{id}/tasks/{task_id}/steer` in
[`api/openapi.yaml`](../../api/openapi.yaml) and `SteerTask` in
[`api/holon/v1/agent.proto`](../../api/holon/v1/agent.proto), with two new events,
`task_steer_queued` and `task_steered`. Extends
[ADR-0102](0102-an-agent-session-is-a-contract-first.md). Queueing is **not** built here,
on purpose (see *What this does not do*); [ADR-0112](0112-a-busy-session-can-queue-a-task.md)
builds it, with the `when_busy` and the status this ADR said it would need.

## The problem

A session runs one task at a time (ADR-0072, ADR-0102), and `SendTask` on a busy session is
`409 task-running`. A person who sees the agent heading the wrong way, or who has one more
thing to say, can wait for it to finish or `CancelTask`, which fails the task and with it
the tool call in flight and the model turn that asked for it. The agent loop is inside
`comp-agentd`, so no client can put a word into it from outside.

## What was read

[pi-durable](https://github.com/earendil-works/pi/tree/main/packages/durable) has three
answers for input to a busy conversation: queue it as a follow-up (its default), *steer* it
in at a tool-round boundary, or reject it. Its write-up is explicit that steering "does not
interrupt a tool that is already executing". What was read is the write-up, not the source.

## The decision

**`SteerTask` adds a message to the running task.** It is input to the task that is
running — not a new task, so it does not go around one at a time — and it is read by the
model at the task's next **model-call boundary**: after every tool call the model asked
for has an answer and before the next model call begins. It enters the conversation as a
user message. Two consequences, both tested:

- A tool is never interrupted. A steer that arrives while a tool call waits on a person
  waits with it, and is read after the tool's result.
- It is read after the model's *final* answer too. A task whose model has just stopped
  writing is not finishing if a steer arrived while it wrote: it takes another turn on it.

**Accepting a steer promises that the model will read it, or says no.** "Applied or
refused, never accepted and dropped" is the property. It needs a guard, because the loop
decides to finish at one instant and a steer can arrive at the next: `Session::finishing`,
called under the same lock as `steer`, either hands back the steers that arrived (the task
has new input; apply them and go round again) or marks the task finishing, after which
`steer` answers `409 task-finishing` and the caller sends a task. A unit test races four
producers against the loop fifty times and checks that every steer accepted was one the
loop read; breaking the guard fails it.

**Two events keep the log honest about where input entered.** `task_steer_queued` when it
is accepted, `task_steered` when it joined the conversation — the point a replay sees it
enter, and what moves it from `pending_steers` to the transcript in the state
([ADR-0107](0107-a-client-catches-up-from-a-snapshot-then-tails.md)).

**Refusals say which.** `404` for no such session or task; `400` for a blank message;
`409` `task-not-running` (it ended, or is not the running task), `session-closed`,
`task-finishing`.

**An `Idempotency-Key` makes a retry say it once**, as `SendTask`'s does. The keys, and the
queue of steers, live only as long as the task — a restart fails a running task anyway
(ADR-0102), so nothing here is saved.

A steered turn is an ordinary turn: it costs tokens, the budget check runs before it, and
it counts against `max_turns`.

## What this does not do

- **No queue.** pi-durable's default, and what `SendTask`'s `409` pushes a client to
  build, is "run it after this one". `agent.proto` carries `reserved 1; // was QUEUED: a
  task starts the moment it is accepted` — a queued state was designed out once, and
  steering does not need it back: a client that wants a follow-up waits for `task_completed`
  or `task_failed` on the stream it already has and then sends. What a client cannot do
  from outside is steer, which is why that is what was built. If queueing is wanted it
  is a `when_busy` on `SendTask` and a status that has to be un-reserved; that is a
  decision for whoever owns the contract, not a by-product of this one.
- **A steer the task never reads leaves no trace beyond the task ending.** If the task
  fails or is cancelled with a steer still queued, no event says it was dropped; the state
  stops listing it as pending. The caller resends it as a task.
- **No steering of a task that is not running**, and none of a particular position: it
  goes in at the boundary, not "now".

## Proof

[`agentd/tests.rs`](../../reconciler/daemons/agentd/src/tests.rs), through a real router with
the scripted model: a steer sent while a task waits on an approval is accepted as the
running task, queued, visible as pending beside the approval in the state at the stream's
cursor, read only after the approved tool's result, and answered by the model's next turn;
every refusal; a key said three times is read once; the guard, singly and under a race;
the same over gRPC, including the new enums. Run by hand against the real binary: the
steer entered the transcript at seq 10, after the tool call's result at seq 5.

## Alternatives

- **`when_busy` on `SendTask`.** Rejected: one field would mean three different things
  (start, queue, inject), and the response — a `Task` — cannot say which happened.
- **Interrupt on send.** Rejected: it kills the tool in flight, which is what `CancelTask`
  is for and what this exists to avoid.
- **Queue inside the daemon.** Not built, above; and it would need its own answer for what
  a restart does to work that was accepted and never started.
