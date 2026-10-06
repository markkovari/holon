# agent-matrix

A Matrix bridge for `agent-runtime`: **every agent is a Matrix user, every project
is a Space.** You use Element X on the iPhone and iPad and Element on the Mac;
there is no app of our own to publish.

```
Element ── Synapse (malna, tailnet) ──appservice──▶ agent-matrix ──HTTP──▶ agent-runtime ──▶ agents
```

## What you get

* **A room per agent.** `@agent-rower:<server>` has a direct room with you, already
  in your room list (the bridge accepts your invites for you). Write, it runs, the
  answer comes back as that agent, with a typing indicator while it works.
* **A Space per project** (`rowing`, `nutrition`, whatever you start): a `general`
  room where you talk to the project's agents and a `feed` room for what they report.
* **Adding an agent to a project is an invite.** Invite `@agent-coach` to the
  project's room and it joins the project; kick it and it leaves. Or use commands
  (below), which work from clients that cannot create Spaces.
* **Membership is the grant.** In the runtime, project members get the project's
  shared store (`project.<name>`), may emit to `<name>.*`, and are told who else is
  in the project. Removing an agent takes all of it away at its next run.
* **Who answers in a project room:** the agents you mention (Element's mention
  picker, or `@name` in the text); if you mention nobody, the project's *lead*; if
  there is no lead, nobody. So five agents do not all answer every message.
* **Approvals are polls.** When an agent needs a human to approve a tool call, the
  bridge posts a poll in its room; your vote answers it.
* **Only you.** Messages from anyone but the owner start no run, even in a room an
  agent is in.

## Control room

A room called **holon** is created for you. Say `!help`:

```
!agents                                  list the agents
!project list                            list projects and who is in them
!project new <name> [description]        make a project (a Space with a general and a feed room)
!project add <name> <agent> [<agent>…]   add agents to a project
!project rm <name> <agent> [<agent>…]    take agents out
!project lead <name> <agent|none>        who answers when nobody is mentioned
!project delete <name>                   forget the project (its rooms stay)
```

## Setup

The runtime must be running (`agent-runtime --state-dir …`) and Synapse deployed
(`infra/matrix/`). Your Matrix account must be a Synapse admin (the account
`spike.sh setup` / `register_new_matrix_user --admin` creates).

```sh
# 1. once: tokens, one owner login, the config and Synapse's registration file
HOLON_OWNER_PASSWORD=… agent-matrix init \
    --server-name malna.tail3a9c.ts.net --homeserver https://malna.tail3a9c.ts.net \
    --runtime-token-file <runtime state dir>/admin-token \
    --appservice-url http://<this machine's tailnet IP>:9009
# 2. give Synapse the registration file (this restarts Synapse for a few seconds)
APPSERVICE_REG=~/.holon-matrix/holon-agents.yaml infra/matrix/deploy.sh Malna
# 3. run the bridge
agent-matrix run
```

`init` writes `~/.holon-matrix/bridge.json` (mode 0600; it holds the appservice
tokens, the runtime's admin token and your session token) and `holon-agents.yaml`.
**Neither goes in the repo.** The registration claims only `@agent-.*:<server>`.

## How it stays consistent

The runtime's registry is the source of truth for agents and projects. The bridge
keeps only a map of which room is which (`state.json`); every pass it reads the
runtime and makes Matrix match: it creates missing rooms, invites the project's
agents, and kicks agents that left the project. Membership changes made *in Matrix*
(an invite, a kick, a command) are written to the runtime first, then reconciled.

## Tested

`cargo test` runs unit tests (mention routing, command parsing, the registration
file, ghost ids). `cargo test --test e2e` is the whole path against a **real
Synapse in Docker** deployed by the same `infra/matrix/deploy.sh` that goes to
malna, with the bridge and a runtime with scripted agents in-process, playing the
owner with plain Matrix client calls. It covers rooms and owner placement, chatting,
Space creation, adding agents by command and by invite and removing them by kick,
mention/lead/silence routing, an approval answered by a poll vote, and a
non-owner being ignored. It skips when Docker is not available.

## Not built yet

* Files in either direction (Matrix media to the agent's workspace and back).
* Agents posting unprompted: a `notify` capability and delivery of scheduled
  results to the project's feed room.
* More than one human, and per-room permissions.
* Running as a service (launchd); today it is a foreground process.
* Whether the bridge's own session and the appservice survive a Synapse upgrade is
  untested.
