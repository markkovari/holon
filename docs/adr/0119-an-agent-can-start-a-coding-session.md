# ADR-0119 — An agent can start a coding session

*`comp-agentd` is where an agent that edits files and runs commands lives, and `agent-runtime` is where
agents that plan, wake each other and remember live. The first thing that makes them one system is a
tool.*

**Status: accepted, and built** for the tools, the watching and the approvals
([`agent-runtime/src/session.rs`](../../agent-runtime/src/session.rs)), and for the board that draws it
([`gpui-console/src/board/`](../../gpui-console/src/board/)). What is not built yet is at the end.
Until this, nothing in the repository was a client of the daemon of
[ADR-0102](0102-an-agent-session-is-a-contract-first.md); `agent-runtime` is the first.

## The decision

**An agent that is granted `session_start` can open a session on the daemon with a task.**
`session_start {task, workspace}` returns a session id at once; `session_result {session, wait_secs}`
says how it stands (still running, waiting for a person, or its answer and what it cost) and waits up to
`wait_secs` (at most 60) for it to end; `session_close` stops it. An agent that starts several and then
collects them runs them side by side. `session` may be left out when it has only one.

**The agent chooses the task. Everything else is the spec's.** `AgentSpec::sessions` (`SessionPolicy`)
says which directories besides the agent's own workspace a session may be opened in (the agent names one
of them, or `own`; it never writes a path), the model, a dollar ceiling the daemon enforces, which of the
session's four tools run without asking (none, by default), and how many sessions may be open at once. A
model asked to code unattended is trusted with the *what*; where, with what and for how much are
decisions of whoever wrote the spec, and the spec is not a tool argument. The runtime says where the
daemon is (`--agentd-url`, `--agentd-token-file`, `--session-model`) and answers "not available" when it
does not.

**The runtime watches the session, and says so.** For each session it started it keeps a view,
`SessionView`, read from the daemon's state snapshot ([ADR-0107](0107-a-client-catches-up-from-a-snapshot-then-tails.md))
every 700 ms: its phase, what it has cost, the tools it has used, what it is doing now, and its parent
in the trace (the run and the tool call that started it, also sent to the daemon as labels). A change
is an `Event::Session` carrying the whole view, which a consumer replaces by id. A daemon that stops
answering ends its sessions as failed, not running for ever. Polled, not streamed, for the reason the
console's run view polls: it is one request and no reconnect logic, and it is the document a stream
would resume from.

**A session's approvals are the runtime's own.** What the daemon is waiting on becomes a `Pending`
(`tool: session:write_file`, the agent that started it, the session in its chain), announced as
`ApprovalRequested` and answered through `resolve_approval`, which now also takes how far a yes reaches
(`once`, `task`, `session`, [ADR-0114](0114-an-approval-can-reach-beyond-one-call.md)). So every place that
already shows an approval shows these with no change: the console's, the chat bridge's polls,
`GET /approvals`. A denial reaches the daemon with its reason, an answer the daemon will not take leaves
the approval where it is, and `GET /sessions` lists the views.

**A window can draw it as it happens.** `Event::RunStarted` carries the model and the parent's span, and
`Event::ToolStarted` announces each call before it is made, so what an agent woke hangs under the call
that woke it while both are still going. The GPUI console draws this as the *agent board*: a tree of
cards, each saying which model, what it was asked, which tools and what it is doing, what it has cost
and for how long, with the ones that need a person in the one colour that asks. Its tree is pure and
built from these events, so what it shows is what the runtime said.

## What it is checked against

Eight tests against a stand-in daemon that speaks `api/openapi.yaml` (the whole flow with an approval,
denial and scope, the workspace and concurrency limits, no daemon, another agent's session id, close, a
daemon that goes away), and one against the real `comp-agentd --provider mock` (`cargo test --test
sessions -- --ignored`: the daemon asks about `list_dir`, a person approves it, the agent gets the
listing). In the real console, on an isolated state directory: an agent hands a task to a real daemon,
its card turns orange and says `waiting to list_dir`, and ends Done with the workspace's listing.

## What it does not do yet

- **A goal from a chat client.** On the board a person sets one up: the list down the left is the
  sessions (a goal and everything it started, newest first, kept six hours once over and seeded from the
  runs the runtime kept, so a restart does not empty it), and *New goal* gives one to an agent and follows
  the session it starts. A chat client cannot do that yet.
- **A window on another machine.** The console embeds its runtime, so a phone or a second computer
  sees nothing of it. The runtime has to stream the same events over HTTP (a bearer token, SSE, as the
  daemon does) for a board, a web page or the chat bridge to attach to one runtime; until then the chat
  bridge, which already talks to the runtime over HTTP, is the way in, and it carries approvals.
- **A daemon the runtime does not start.** The console starts one (the mock provider unless the operator
  names a real one: a console that began spending an API key the first time an agent asked for a coding
  session would be a surprise), or attaches to one; the headless runtime is told where it is.
- **A session's transcript.** The board says what a session is doing and how it ended. What it said and
  wrote is in the daemon's log, and in the pull request the work becomes.

## Alternatives

**Make the agent's own loop the coding agent.** `agent-runtime`'s file tools and loop could be grown
until they do what the daemon does. That is the second copy of a loop that
[ADR-0117](0117-an-autonomous-run-is-a-task-in-a-session.md) is about, and none of what the daemon has
built since 0102 would come with it: streamed events, approvals that outlive the process, steering.

**Let the agent pick the workspace and the model.** Shorter to write, and a way for an agent a person
talked into it to open a session on anything on the machine, or on the dearest model there is.

**A webhook from the daemon in place of polling.** One request less per second, and a second kind of
thing for the runtime to serve and the daemon to be told about. The snapshot every client already uses is
cheaper to be wrong about.

## What it costs

The daemon is one more process, and one more token. A session's snapshot grows with its transcript and
is read in full each time, which is fine for a session that is minutes long and is what the cursor in it
is for when it is not. And an agent's own run and its session are two budgets, tokens and dollars, that
nothing adds together.
