# ADR-0135 — The Matrix bridge is a component

*`agent-matrix` was native for no capability a component lacks: it spoke HTTP to Synapse and to the
runtime, kept a JSON file, and used threads to wait on minute-long runs and to follow the runtime's
event stream. A run is now a job the runtime keeps, the file is a document in the app's keyvalue,
and the host's pump is the loop. The logic stays in the `agent-matrix` crate; `matrix-bridge` runs
it on comp-host, and the binary goes.*

**Status: accepted.** Revises how ADR-0105's bridge runs; what it does (agents as users, projects
as Spaces, polls for approvals, retiring rooms) is unchanged.

## The problem

ADR-0095 allows a native program only for a capability WASI does not give a guest. The bridge used
three things a component could not have: a run started with `wait: true` blocked its thread for as
long as the agent took (minutes, behind an approval); a thread followed `/v1/events` to learn when
to post polls and scheduled results; and a thread slept five seconds between reconciles. None of
these is a capability: each is a way of waiting.

## The decision

- **A message starts a job** (`wait: false`, the runtime's `/v1/jobs/{job}`, added for this). The
  bridge records what it owes as a `Pending`: the room, who answers, the "thinking" message the
  answer replaces, whether a voice answer is also spoken, the bare task to ask again when a run with
  a follow-up photo fails, and a steer directive's text. The answer is posted by whoever finds the
  job finished and takes the record out first.
- **The pump is the loop** (ADR-0130, every 2 s): reconcile, post and settle approval polls,
  deliver scheduled results, deliver finished jobs. The event stream is not followed: a pump pass
  is at most two seconds behind it, and it already caught up on a reconnect by doing the same.
- **State is one document in the app's keyvalue, every write a compare-and-set** (`comp:store/cas`):
  a Synapse transaction and a pump pass may run at once. What lived in memory moved into it — the
  recent event ids (a resent transaction is not acted on twice), the absence counts that guard
  retirement (ADR-0105 §3), whether the owner was told of a refused retirement, the pending runs.
  A pass takes a lease key first, so a hand-sent pump cannot run beside the host's.
- **The crate keeps no I/O.** `Http` and `Store` are traits; the component hands it `wasi:http` and
  the keyvalue, the e2e suite hands it reqwest and a file, with a thread standing in for the pump
  and tiny_http for comp-host. `appservice::handle` is the request handling both serve.
- **Setup is the CLI's.** `holon matrix init` logs the owner in and writes the app's config file
  (`bridge.conf`, the secrets in it, 0600) and Synapse's registration. `holon matrix migrate` turns
  the native bridge's `bridge.json` into it with the same tokens, and `import-state` hands the old
  `state.json` to the component, which takes it only while it has none — so every room survives.
  The GPUI board reads the rooms from `/internal/rooms` with the hs_token it already could read.

## Consequences

- One more component on comp-host and one less native binary; launchd's `bridge` is `comp-host`
  with `--pump`, `--kv sqlite` and `--config-file`. Egress is `'*'` with private addresses allowed:
  the homeserver is on the tailnet and the runtime on loopback, and both are config, not code.
- A Synapse transaction is answered when its events are handled, as before; what is slow (a run) is
  a job. A voice note is still transcribed inside the transaction: a long one can outlast the
  host's 30 s request timeout, and Synapse's retry is then dropped as already seen. *Shortcut:
  transcription inline; make it a job like a run when voice notes get long.*
- The suite found two bugs the native bridge had: an agent's earlier answers reached its history as
  "*(thinking...)*" (an edit was skipped, not applied), and the owner, rate-limited by Synapse while
  joining a burst of new rooms, was left with an invite for good. History now reads a message as
  its latest edit, and a rate-limited room is joined again by a later pass.
