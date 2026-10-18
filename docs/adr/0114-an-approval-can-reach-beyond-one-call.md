# ADR-0114 — An approval can reach beyond one call

*A person asked the same question about the same tool a third time in one task has not learned
anything since the first, and neither has the daemon.*

**Status: accepted, and built.** `scope` on `SubmitApproval` and on `tool_approval_resolved`,
`approved_tools` on the session, and `--auto-approve-tool` on `comp-agentd`
([`reconciler/src/bin/agentd/`](../../reconciler/daemons/agentd/src/): `wire.rs`, `session.rs`,
`agent.rs`, `service.rs`; `api/holon/v1/agent.proto`, `api/openapi.yaml`). Extends the approval of
[ADR-0102](0102-an-agent-session-is-a-contract-first.md), and works the same whether the wait is
held in memory or parked ([ADR-0113](0113-a-task-waiting-for-a-person-is-a-parked-call.md)).
Additive: a client that sends no scope, and a daemon started with no flag, behave as before.

## The problem

A tool call that is not in the session's `auto_approve_tools` asks a person, every time. A task
that reads five files asks five times; one that writes three asks three times, and each is a
person's minute. The only way to stop is to list the tool when the session is *created*, which is
before anyone knows which tools this task will want, so the choice a caller has is to ask about
everything or to allow it in advance, blind.

The person who can decide is the one looking at the call. The first time they see `write_file` on a
path they are happy with, they know whether the next one is the same kind of thing, and the
protocol gave them no way to say so.

## The decision

**An approval says how far it reaches.** `SubmitApproval` takes a `scope`:

| `scope` | covers |
|---|---|
| `once` (what a client that does not say gets) | this call; the next call of the tool asks again |
| `task` | this call and the rest of this task's calls of the tool |
| `session` | this call and every later call of the tool in this session, in any task |

It is for a **tool**, not for one call's input. Approving `run` for the session approves every
command, which is exactly what creating the session with `auto_approve_tools: ["run"]` would have
done, decided at the moment someone is looking at a call instead of before. A client should offer
it as "always allow `run`", never as "allow this command".

**Only an approval has a reach.** A denial is for the call it answers. `deny` with `task` or
`session` is refused (`400 bad-request`, gRPC `INVALID_ARGUMENT`), not quietly narrowed, so that a
person who meant "never this tool" finds out they did not get it, with the call still waiting. And
whatever a ticket says (a ticket has other writers), the daemon reads a denial as `once` and logs
it so: nothing here ever remembers a refusal.

**The answer says what was understood.** `SubmitApprovalResponse` carries the `scope`. A daemon from
before this ADR ignores a field it does not know, so a client could believe its "remember this" was
taken; the missing `scope` in the reply is how it finds out.

**It is remembered when the answer is decided, and the log says so.** `tool_approval_resolved`
carries the `scope`, and the tool is remembered under the same lock that appends that event, so what
the log says was approved and what runs unasked cannot differ. For a parked approval "decided" is
the delivery, as in ADR-0113: an answer that has gone into the ticket and not come back is said and
not yet decided, and nothing is remembered until it is.

**It is asked at each call.** Whether a call runs unasked is looked at when the call comes up, not
once for the step. A step that has two calls of one tool needs one answer, and a parked step that
resumes goes on to its next call with the answer it was just given already in force.

**It is derived from the log, like everything else a session knows.** A restart reads back the
`session` approvals of every tool the log shows was asked about, and, for a parked task it restores,
that task's `task` approvals. No new file. Only an `approve` is read as one, so a log that says
otherwise (an older daemon's, which has no `scope`, or one that was edited) does not make a refusal a
standing approval, and an approval of a call the log never shows asked about is not remembered,
because there is no tool to remember.

**It is visible.** `approved_tools` on the session: the tools a person approved beyond one call, the
session's and, while it runs, the task's. It is apart from `auto_approve_tools` on purpose. That one
is what the session was created with and is echoed as it was given; a client that copies it to
create another session must not carry one conversation's approvals into the next.

**An operator can allow a tool for every session it creates.** `--auto-approve-tool <tool>`
(repeatable; `list_dir`, `read_file`, `write_file` or `run`, and a typo is refused at start rather
than found out when a person is asked) is added to a session's `auto_approve_tools` when the
session is created. So a session keeps what it was created with: starting the daemon again with a
different list changes sessions created from then on, not the ones that exist, neither widening
nor narrowing them. `run` cannot be confined to the workspace, so allowing it here lets every
session's agent run any command; the flag's help says so.

## What this does not do

- **There is no revoke.** An approval for the session lasts until the session is closed (and a
  daemon restart does not end it: it is in the log). A task's ends with the task. To take one back,
  close the session and create another. A `DELETE` is an additive change if it is wanted.
- **It does not look at the input.** `write_file` for the session is any path the file tools allow,
  which is anywhere inside the workspace; `run` is anything. A scope narrower than a tool would need a
  pattern language over shell commands, which is a security feature of its own.
- **There is no "never".** A standing denial is a policy and not an answer. The policy knob is
  `auto_approve_tools`, where leaving a tool out is "ask".
- **It does not say, in the transcript, why a call ran unasked.** A call under a remembered
  approval has `tool_call_started` and `tool_call_finished` and no approval events, like one in
  `auto_approve_tools`. The approval that covers it is the earlier `tool_approval_resolved` with its
  `scope`, and `approved_tools` says what is in force now.
- **Approvals of one step are still asked one at a time.** Asking for all of them at once
  ([ADR-0113](0113-a-task-waiting-for-a-person-is-a-parked-call.md)'s next step) is not done.
- **A consumer that matches `tool_approval_required` to `tool_approval_resolved` still can**: every
  approval that is asked is still answered, and the new ones simply are asked less often.

## Proof

[`agentd/tests.rs`](../../reconciler/daemons/agentd/src/tests.rs), driving the real router over HTTP and
gRPC, with a scripted model where a step needs several calls, and `FakePark` where an approval parks:

- **Session.** One answer stops the questions about that tool in later tasks of that session, and
  not in another session on the same daemon; the reply and the logged event say `session`, and the
  session lists it in `approved_tools`, apart from `auto_approve_tools`.
- **Task.** The second call of a tool in the same step runs with nobody asked (announced and
  finished, no `tool_approval_required` between); the next task is asked again, and the session lists
  nothing once the task is over.
- **Once.** Not saying and saying `once` ask again; a denial is asked about again, and is logged `once`.
- **A denial has no reach**: `deny` with `task` or `session`, over REST and gRPC, and a scope nobody
  defined (a string, or `99`), are refused and the call is still waiting.
- **One tool is not another**: approving `list_dir` for the session, the `read_file` in the same step
  is still asked about.
- **A restart** reads the session's approvals back, and not a task's or a `once`; what the session
  says is what then runs. A parked task restored by a restart has the approvals it was given for the
  task before it, so the model's next call of that tool is not asked, and it remembers the tool the
  parked call was for, not the first of its step.
- **Parked.** The reach rides in the ticket with the answer, and is nothing until the answer is
  delivered; a task approval covers the rest of the step with no second ticket; a session approval
  means the next task parks nothing. A ticket that says deny for the session (it is not reachable by
  `SubmitApproval`, so it is another writer's) is a denial and remembers nothing, and so is an
  answer from a daemon that did not know scopes.
- **The log.** What a restart remembers from a log, case by case: an approval for the session of the
  tool that was asked; several, in order; a log from before scopes, `once` and `task` (nothing); a
  refusal with a reach (nothing); an approval of a call never asked about (nothing).
- **The flag.** Each session created gets the daemon's tools after the caller's own, once; a session
  keeps what it was created with when the list changes; only a tool the daemon has is accepted, and
  the error names them.

Mutation checks: thirty-six, each breaking one behaviour, each caught, each restored from a backup,
among them a session or task approval ignored when asking; one reach remembered as the other; a denial
keeping its reach or the log always saying `once`; a restart forgetting the session's approvals,
forgetting a parked task's, reading a refusal, a task's or a `once` as the session's, or remembering
the call id instead of the tool; the view leaving out the task's or the session's; a parked answer
remembering the first call's tool; the in-memory answer losing the tool; a denial with a reach
accepted; the daemon's allowance not added to a session or added twice; the reach dropped on its way
into the ticket or out of the delivery; the reply always `once`; every gRPC mapping, an unspecified
reach read as the session's and an unknown one guessed at; an unspecified reach defaulting to the
session; an answer that must name its reach; a log from before reaches that cannot be read.

**Through the real daemons**
([`reconciler/tests/agentd_park.rs`](../../reconciler/tests/agentd_park.rs), run locally like the
others there: it spawns `nats-server`, which CI does not): a task parks, a person approves the tool
for the session, the reach goes through `comp-park`, NATS and `comp-resume` and back; `kill -9` of
the daemon; its replacement has the approval from the log, and a second task finishes with nobody
asked. And a daemon started with `--auto-approve-tool` creates a session that lists it and runs the
tool unasked. The one mutation the unit tests cannot see, the flag not being handed to the daemon,
is caught there.

## Alternatives

- **Only an operator's list, nothing in the answer.** Rejected as the whole answer: the list is
  decided before anyone has seen the task, and the person looking at a call decides with what they
  can see. Kept as the second half, because "reading never asks" is the easy case for it.
- **Add the approved tool to `auto_approve_tools`.** Rejected: that field is what the session was
  created with, so a client that copies it would carry one conversation's approval into the next, and
  a restart would need a second place to keep it besides the log.
- **A reach by input pattern** (`run` for `ls *`). Rejected here, as above.
- **An event for every call that ran under a remembered approval**, to say why. Rejected: a new line
  on every stream, and a changed vocabulary for the clients that pair `required` with `resolved`. The
  earlier `resolved` and `approved_tools` say it.
- **Narrow a denial's reach to `once` and say nothing.** Rejected: the client's assumption was wrong,
  and a `400` is how it learns that.
