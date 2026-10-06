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
* **Make a Space in Element, invite an agent into it: that is a project.** Element X
  has *Create space*. The bridge adopts the Space as a project named after it
  (`Nutrition & Meals` becomes `nutrition-meals`; the Space's topic becomes the
  description), with the invited agent as its first member, and says so in the control
  room. Invite more agents to the Space *or to any room in it*; kick one and it leaves
  the project and every room in it. Rooms you add to the Space later are picked up
  within a few seconds, with the members already in them.
* **Or let the bridge make it:** `!project new rowing` creates the Space with a
  `general` and a `feed` room (commands below).
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
file, ghost ids). `cargo test --test e2e` is the whole path (two scenarios) against a **real
Synapse in Docker** deployed by the same `infra/matrix/deploy.sh` that goes to
malna, with the bridge and a runtime with scripted agents in-process, playing the
owner with plain Matrix client calls. It covers rooms and owner placement, chatting,
Space creation, adding agents by command and by invite and removing them by kick,
mention/lead/silence routing, an approval answered by a poll vote, a non-owner being
ignored, and — in the second scenario — a Space made in the owner's own client being
adopted as a project, its rooms followed (including ones added later), an agent added
by inviting it to a room inside the Space, removal by kick, and a Space whose name has
nothing usable being refused with a note. It skips when Docker is not available.

## Limits

* A Space's project name is its ASCII letters and digits (`slug`): `Táplálkozás` would
  become `t-pl-lkoz-s`. A name with none is refused with a note; rename the Space.
* The bridge manages rooms in an adopted Space **as you** (it holds your session), since
  it is not a member of rooms you made. It only invites and removes agents there.

## Not built yet

* Files in either direction (Matrix media to the agent's workspace and back).
* Agents posting unprompted: a `notify` capability and delivery of scheduled
  results to the project's feed room.
* More than one human, and per-room permissions.
* Running as a service (launchd); today it is a foreground process.
* Whether the bridge's own session and the appservice survive a Synapse upgrade is
  untested.

## Who answers when nobody is named

In a project room, `@name` picks the agent. Otherwise, if `HOLON_EMBED_URL` points at the
embedding service (`agent-runtime/embed/server.py`), each member's description is compared with
the message and a member answers only if it fits clearly better than the others. Otherwise
(no service, or a near tie) the project's lead answers.
