# ADR-0121 — A session's edits are in the code store

*An agent rewrote four files over twenty turns, and the only record of what it did is the files.*

**Status: accepted, and served** by `comp-agentd --vcs-url` against `comp-vcs`.

## The problem

A coding session (ADR-0102) has an ordered event log: every tool call, its input and its output.
What it does not have is the state of the workspace at any point in that log. "What did the
agent change in this task", "which call wrote this line", and "undo everything since that
message" each meant reading `write_file` inputs out of the log by hand, and the last could not be
done at all once `run` or a person had touched the files too.

Hosted tools like Delta (delta.dev) solve this for their own agents, on their own servers: a
CRDT history of every edit, linked to the conversation. We already have the store for it, the
code store of ADR-0099 (an op log over a symbol-level patch graph, every edit an op, `revert-op`
appends the inverse), and an agent that should be using it.

## The decision

- **One workspace per session**, named by the session id, one component (`workspace`). Each
  `write_file` that succeeds is `ingest-file`d: the store splits the file into symbols (a file it
  does not parse is one `File` symbol) and commits a patch per changed symbol. The ops are
  tagged with the agent `agentd:<session>/<task>/<call>` and the session's model.
- **What was on disk first is recorded first**, as the agent `agentd:workspace`. A change made
  outside `write_file` (by `run`, or a person) is then the workspace's own, not taken for the
  agent's, and is not undone as if it were.
- **The log links the two**: a `file_changed {call_id, path, ops}` event, between the call's
  start and finish. Additive: a client that does not know the kind ignores it.
- **Rewind** (`POST /v1/sessions/{id}/rewind {after_seq}`, `RewindSession`) reverts the ops the
  `file_changed` events after `after_seq` name, newest first, skipping ops an earlier rewind
  reverted; reads the files back from `snapshot-export`; writes them, or removes one the agent
  created; and records `session_rewound {after_seq, ops, paths}`, a session-level event. The
  conversation is not rewound: the model still remembers writing what is gone.
- **The engine learned to undo a stack** (ADR-0099, *Revert*): a later op that a later revert
  undid, and that revert, no longer block a revert. Before, undoing two edits of one symbol
  newest first was refused at the second.
- **Off unless asked**: without `--vcs-url` nothing is recorded and a rewind answers
  `409 edits-not-recorded`. A record that fails is logged and the edit stands; only a rewind past
  it is lost.

## What it does not do

- `run` is not recorded: a shell's writes are not a list of files. They show up as the
  workspace's own the next time `write_file` touches the same file.
- A rewind past a change made outside `write_file` is refused (`409 rewind-refused`) at the
  first op it would undo under it; what was reverted until then stays reverted and recorded.
- A task sent while a rewind runs races it (the check that no task runs is not held).
- Git stays the handoff: `materialize` or `snapshot-export`'s `git_tree` turn a session's
  workspace into a tree to commit, as for any other code-store workspace.
