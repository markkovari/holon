# ADR-0113 — A task waiting for a person is a parked call

*A wait that lives in a future dies with the process that holds it, and a person who has not
answered yet has done nothing wrong.*

**Status: accepted, and built.** `--park-url` on `comp-agentd`, and `POST /park/deliver`, the sink
`comp-resume` delivers to ([`reconciler/src/bin/agentd/`](../../reconciler/daemons/agentd/src/):
`park.rs`, `agent.rs`, `session.rs`, `service.rs`). Builds what
[ADR-0106](0106-a-resumed-turn-knows-what-it-may-repeat.md) left unbuilt ("`comp-agentd` parks
nothing"), on the contract of [ADR-0100](0100-a-parked-turn-is-a-record-not-a-held-thread.md) and
the resumer of ADR-0106. Opt-in: nothing changes for a daemon started without the flag.

## The problem

A task that needs a person's approval (a tool not in the session's `auto_approve_tools`) waits on a
channel in memory, inside the agent loop. A restart of the daemon ends it: `Session::load` fails
the running task, `internal`, "comp-agentd restarted while this task ran", and the conversation
of that task, which was written only when the task ended, is gone with it.

That wait is the longest thing the daemon does. A model call is seconds and a tool is bounded by a
two-minute timeout; this one is bounded by a person's day, and so it is the one most likely to
span a deploy, an OOM or a reboot. The person who answers after it finds the task cut off, and the
approval they were asked for was asked for nothing.

## The decision

**It is opt-in.** `--park-url` (the base URL of `comp-park`, with `--park-token` or
`--park-token-file`) needs `--state-dir`. Without it the wait is a future held here, as it always
was, and a restart ends the task.

**A wait for a person is a parked call.** When a tool call needs a human, the loop writes down
where it stands and then lets go:

1. the conversation (`history.json`), and a checkpoint (`checkpoint.json`): the task, which model
   turn it is on, the calls the model asked for in this step, which of them is waiting, the
   results of the ones before it, and when to give up;
2. `tool_approval_required`, in the log;
3. a ticket in `comp-park`: correlation the call's id, session the session's id, `replay: unsafe`,
   deadline the approval timeout if there is one.

Then the loop returns. The task is *parked*: running, as far as the queue and the API are
concerned (`active_task_id`, session status `awaiting_approval`), and with no future, no channel
and nothing in memory beyond the checkpoint.

**The answer goes to the ticket, and its delivery moves the task.** `SubmitApproval` gives the
person's decision to the ticket and does nothing else; the task does not move. `comp-resume`
delivers it to `POST /park/deliver`, which logs `tool_approval_resolved` and starts a new loop from
the checkpoint, with the call's answer in hand. That is one path whether or not the process
changed, so the restart is not a special case that is exercised only when it happens. It costs a
round trip through `comp-park`, NATS and `comp-resume`: measured at 0.02 s from the answer to the
task finishing.

**The checkpoint is taken when the answer is.** Resuming takes it, removes it and logs the
decision under one lock. A delivery that comes twice then finds the call already decided and is
taken and ignored, and a crash after that finds no checkpoint, so the task is *failed*, not
resumed. That is deliberate: the tool the answer unlocked may already have started, and running it
again would run a side effect twice, which is what `unsafe` means in ADR-0106.

**A restart parks again what it finds.** `Session::load` restores a parked task when its checkpoint
is readable, is for the open task, names a call that exists, and the log shows that call asked
about and not decided. Anything else is a task cut off in the middle of something, and is failed
as before. Its spend, and the steers it had not yet read, come back from the log. And the daemon
parks every approval it finds parked again, which is idempotent by correlation, so a crash between
the checkpoint and the ticket, or a store that lost the ticket, leaves nothing a person can answer
into the void: `SubmitApproval` makes a missing ticket and answers it.

**A timeout is the ticket's deadline.** `--approval-timeout-secs` becomes the deadline. When it
passes, `comp-resume`'s recovery pass interrupts the ticket and the sink reads that as a denial,
"nobody answered in time", decided by `timeout`, as a timeout always was. The precision is the
resumer's: its grace plus its sweep. Measured with `--sweep-secs 1 --deadline-grace-secs 0`, a
3 s timeout was a denial at 3.4 s; with the resumer's defaults, 10 s and 15 s, it can be up to
25 s late. There is no timer in the daemon in this mode.

**An ended task takes its ticket with it.** Cancelling a task or closing a session cancels the
ticket, best effort and off the caller's path. One that is missed is answered `410` when it is
delivered, which is what tells `comp-resume` to cancel it from its side. The sink answers `2xx`
for "taken, now or before" and `410` for "there is nobody to take it": no such session, or no task
waiting on that call that was not already decided.

**If it cannot park, the task fails.** Three tries, a second and two apart, then `internal`,
"could not park the approval of `<tool>`". It does not quietly wait in memory: an operator who
asked for waits that survive would find on the day it mattered that they had not.

**If an answer cannot reach `comp-park`, nothing has happened, and it can be sent again.**
`503 park-unavailable`, and the person's claim on the call is released so that the retry takes.
The first to answer a call may; a second is refused, `409 not-pending`, as it is for a wait in
memory.

## What this does not do

- **It does not make the running loop durable.** The checkpoint exists only while a task waits on a
  person. A crash while a tool runs or the model streams still ends the task, `internal`, as it did.
- **It needs `comp-park` and a `comp-resume` that delivers here.** `--sink-url` at this daemon,
  `--sink-token` its `--token`. Without the resumer an answered task stays parked: the answer
  waits in its ticket, and the resumer's sweep announces it again when it is back.
- **The queue is still memory** ([ADR-0112](0112-a-busy-session-can-queue-a-task.md)). A restart
  ends the tasks queued behind a parked one and keeps the parked one.
- **Approvals are still asked one at a time, in the order the model asked for them.** A step with
  several tool calls parks on each in turn, so there is one ticket at a time. Asking for all of a
  step's approvals at once, as one group ([ADR-0109](0109-a-call-may-fan-out.md)), is the obvious
  next step and is not done here.
- **The sink is not in `openapi.yaml` or `agent.proto`.** It is the other end of
  `comp-resume --sink-url`, and sits behind the same token as everything else.
- **Models and tools are not parked.** A model call is a stream; a tool is bounded by its own timeout.

## Proof

[`agentd/tests.rs`](../../reconciler/daemons/agentd/src/tests.rs) plays `comp-park` with a fake that
records what it is told and `comp-resume` by posting what it would deliver: the approval parked
once, for this call and session, as unsafe; the task not moving until the answer is delivered, then
resuming with the turn before the park still counted; a second answer refused and a repeated
delivery ignored; an interruption read as a denial the model is told of; the approval timeout as
the deadline; a tool that needs no approval not parking; a restart taking the answer in the new
process with the spend of the first turn, the steers and the checkpoint all intact, and a restart
that cannot park, or cannot trust the checkpoint (unreadable, for another task, or for a call the
log shows decided), failing the task as before; cancel and close cancelling the ticket and a late
delivery being `410`; an answer that cannot reach `comp-park` being `503` and retryable; a ticket
the store lost being made again; an approval that cannot be parked failing the task after three
tries; a steer, and a task queued behind a parked one; a step with two calls parking on each in
turn and the model handed both results in order; gRPC; and a threaded test, forty rounds, of
deliveries racing each other and a cancel, in which a parked call is resumed at most once, the log
says it was decided exactly as often, and a cancelled task is never resumed.

Mutation checks: twenty-five, each breaking one behaviour and each caught, among them a restart that
never parks, a resume that leaves the checkpoint, one that logs nothing, one that can be taken twice,
a second answer accepted, a cancel or a close that leaves the ticket, a checkpoint trusted for any
task or whatever the log says, a restored task forgetting its spend or its steers, an interruption
read as approval, an unknown session's delivery accepted, a claim kept after a failed answer, no
parking again at start, a lost ticket not made again, no retries, replay `safe`, a resumed step
starting from its first call or without its answer, a conversation not saved, a deadline dropped,
and `410` and `503` as other codes.

**Through the real daemons**
([`reconciler/tests/agentd_park.rs`](../../reconciler/tests/agentd_park.rs), run by hand first
and then kept): `comp-agentd`, `comp-park` and `comp-resume` as processes over a private
`nats-server`, a token on every hop. A task needs a person and parks; `kill -9` of the daemon that
asked; a new one starts on the same state directory and finds it waiting, not failed, with the
approval still pending; a person answers *it*, and the answer goes through `comp-park`, NATS and
`comp-resume` back to its sink, and the task finishes with the spend of both turns. And an
approval nobody answers, with `--approval-timeout-secs 2`, is interrupted by the resumer's pass
and is a denial. Two mutations aimed at what only this can see, the daemon not sending its token to
`comp-park` and the sink served at another path, are caught.

## Alternatives

- **Embed the park engine, with its own NATS connection, in `comp-agentd`.** Rejected: a second
  client of the same bucket and a dependency this daemon does not otherwise have. `comp-park` is
  what components already speak to over loopback HTTP, and so is this.
- **Skip the round trip when the loop is alive in memory.** Rejected: two paths, one of them rare
  and so untested until it matters. One path made the restart the ordinary case, and it costs
  0.02 s.
- **Keep the whole loop's state, as a journal, at every step.** Rejected: a larger change for a
  wait nothing else rivals. The checkpoint covers the one that is long.
- **Wait in memory when `comp-park` is down.** Rejected, above.
- **Refuse to start without `comp-park`.** Rejected: it may start after this daemon, and a parked
  task found at start is parked again until it does.
