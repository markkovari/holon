# ADR-0112 — A busy session can queue a task, and a restart ends what never started

*"One task at a time" is right, and a client that wants the next task to follow this one is left
to watch a stream, wait for an ending and race every other client to resend.*

**Status: accepted, and built.** `when_busy` on `SendTask` in [`api/openapi.yaml`](../../api/openapi.yaml)
and [`api/holon/v1/agent.proto`](../../api/holon/v1/agent.proto), a `queued` task status, a
`task_queued` event, `queued_tasks` in the session state, and `--max-queued` on `comp-agentd`.
Extends [ADR-0102](0102-an-agent-session-is-a-contract-first.md) and builds what
[ADR-0108](0108-a-running-task-takes-input-at-a-boundary.md) said it did not.

## The problem

A session runs one task at a time, and `SendTask` on a busy session is `409 task-running`. A client
that wants "run this next" has to subscribe, wait for `task_completed` or `task_failed`, and send
again: a loop every bot rebuilds, with a gap in it. Two clients that both see the session go idle
send at once, and one gets the `409`.

ADR-0108 declined to build a queue, and named what one would need: "a `when_busy` on `SendTask` and a
status that has to be un-reserved", and "its own answer for what a restart does to work that was
accepted and never started". This is both.

## What was read

[pi-durable](https://github.com/earendil-works/pi/tree/main/packages/durable) has three answers for
input to a busy conversation: queue it as a follow-up (its default), steer it in, or reject it.
Steering is ADR-0108's; this is the queue. What was read is its write-up, not its source.

## The decision

**`when_busy` on `SendTask`: `reject` or `queue`, default `reject`.** A client that does not say
gets what it always got. In gRPC `WHEN_BUSY_UNSPECIFIED` is `reject`, which is what every client
written before the field sends. ADR-0108 turned down `when_busy` because one field would have meant
three things (start, queue, inject) and a `Task` could not say which happened; steering has its own
call now, and with two meanings the response does say: `status` is `running` if it started and
`queued` if it is waiting. On an idle session `when_busy` is ignored and the task starts.

**Accepted onto the queue, a task is `queued` and the log says so.** `202` with `status: queued`,
and a `task_queued` event carrying the prompt. The queue is first in, first out, per session. The
prompt is not in the conversation yet: the transcript shows it when the task starts, and the state
lists it in `queued_tasks`, in order, until then.

**The next task starts in the same step as the end of the one before it.** `finish` and `cancel`
already hold the session's lock to append the ending; the oldest queued task is promoted under that
same lock, and `task_started` is the very next event, one `seq` after the ending (tested, and seen by
hand: `#15 task_completed`, `#16 task_started`). There is no instant at which the session is idle
with a task waiting, so a task that was accepted either starts or ends in the log and is never left
in a queue nothing will drain. The promoted task gets a loop of its own, launched by whoever ended
the one before: a cancel aborts the running task's loop, so the next one cannot live inside it.

**Ending a queued task, and a session.** `CancelTask` on a queued task takes it out of the queue and
ends it `task_failed{cancelled}` with no usage: it never ran. Cancelling the running task starts the
next. Closing a session ends the running task and then every queued one, in order, as `cancelled`;
the session is closed first, under the one lock, so nothing is promoted into the gap and nothing new
is accepted.

**Bounded.** `--max-queued` is per session, 16 by default. One more is `429 queue-full` (gRPC
`RESOURCE_EXHAUSTED`), which a caller can wait out and a `409` is not. `0` switches queueing off: a
busy session refuses as it did, and the refusal says the daemon does not queue.

**An `Idempotency-Key` covers queued tasks**, and keeps finding its task as it goes from queued to
running to ended, and after a restart.

**The budget and `max_turns` are met when the task starts, not when it is queued.** Tested: with a
budget that pays for one model turn and not two, the first task fails on its second turn, the queued
task (accepted while there was budget left) starts, finds it spent, and fails `budget_exceeded` at
once. It was accepted, it started and it ended.

**A restart ends what never started.** The queue is memory; on load it is derived from the log, and a
task that was queued and has no start is ended there, `task_failed{internal}` "comp-agentd restarted
before this task started", after the running task's own `restarted while this task ran`. It is not
run now, on a prompt a process that is gone accepted: its client saw the stream break and may already
have sent it again, and running it quietly would do the work twice. Ended in the log, every reader
finds out and the caller decides; the key it was sent under still finds the ended task, so a retry is
told how it ended and not handed a second copy.

## What this does not do

- **No priorities, no reordering, no jumping the queue.** First in, first out.
- **No queue across a restart.** Above, on purpose.
- **A queued task waits as long as the one before it runs.** An approval nobody answers holds the
  queue, and `--approval-timeout-secs` is the lever that already exists for it. There is no queue
  timeout.
- **A queued task cannot be steered.** `409 task-not-running`, with a message that says it is queued:
  steer it once it runs, or cancel it and send it again.
- **No queue across sessions.** A session is one conversation and one queue.
- **A client with a closed set of statuses may not know `queued`.** Only a client that asked for
  `queue` is ever given one, and `queued_tasks` in the state is an addition (an empty list for a
  session that never queued).

## What found what

A latent bug in `Session::load`, found while writing the restart test. Any task ending in the log
cleared the marker that says "a task is running and was cut off", which could not matter while there
was only ever one task at a time and does once a queued task can end while another runs (cancelled
before the restart). It is fixed, and the restart test cancels a queued task first so that it is
exercised.

## Proof

[`agentd/tests.rs`](../../reconciler/src/bin/agentd/tests.rs), through a real router with the scripted
model: a busy session refusing by default and queuing when asked, in order, with the next task's
start the very next event and the state's cursor continuing exactly through the drain; an idle
session starting whatever `when_busy` says; a queued task cancelled and skipped; the running task
cancelled with the next one starting on a loop of its own and running to its end; a close ending
every task; the bound, and switching it off; a key said twice queueing once and finding its task
after it started; a queued task not steerable; the budget met at the start; a restart ending the
queued tasks, and the key finding the ended one; the same over gRPC, including `RESOURCE_EXHAUSTED`
and the new enums; and a threaded test, forty rounds, in which four producers queue while the loop
ends tasks and a canceller cancels, after which every task accepted has started at most once, ended
exactly once, and the queued ones started in the order they were accepted.

Mutation checks: sixteen, each breaking one behaviour and each caught (an ending that no longer
promotes, a cancel that no longer promotes, a close that leaves the queue, the bound off by one, a
restart that leaves the queue, the load bug above, a cancelled task left in the queue, a promoted
task losing its acceptance time, an idle session queuing, the loop or a cancel not launching the
promoted task, the state keeping a started task queued, gRPC reading "unspecified" as queue, a full
queue as `409`, queueing as the default). The one that matters most, promotion no longer in the same
step as the ending, is caught by the threaded test alone, and in eight runs of that mutation, eight
times.

By hand with the real binary and the mock provider: three tasks queued and drained through their
approvals, each start the very next event after the last ending; a queued task refused a steer; a
key said twice; then `kill -9` with one task running and two queued, and a restart on the same state
directory, which ended the running task and the two queued ones in the log, left the session idle
with an empty queue, and took a new task.

## Alternatives

- **Leave it to the client.** The status quo. Right for a client that wants to decide, and it is
  still available: `reject` is the default.
- **A third value, `steer`.** Rejected, as in ADR-0108: steering names the task it goes to and is
  refused, not queued, if that task is not running.
- **A `QueueTask` call beside `SendTask`.** Rejected: the two share an idempotency key, a refusal
  table and a response, and a client switching between them would be switching on one word.
- **Run queued tasks when the daemon restarts.** Rejected, above.
- **Unbounded.** Rejected: memory, and a task waiting on an approval nobody gives would hold a queue
  with no limit. A bound turns that into a `429` a caller sees.
- **Keep the queue in its own file.** Rejected, as ADR-0107 rejected a second copy of the state: the
  log is the one truth, and the queue is read off it.
