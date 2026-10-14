# ADR-0105 — agents live in Spaces; deleting and archiving have a defined end

*Every agent has its own direct room today, projects are Spaces, and nothing in the
bridge handles an agent that no longer exists. The result is a People list that only
grows and a Matrix user that outlives its agent. This decides where agents live in
Element, what "delete an agent" and "archive a project" mean on the Matrix side, and
what has to be verified before any of it is built.*

**Status: proposed.** Nothing here is built. Written first because the grant model
(membership is permission) constrains the design more than the Matrix side does, and
because "archive" has no Matrix primitive: the first idea for it is wrong in one
case (adopted Spaces, below).

## What exists, read from the code

| piece | where | what it does | gap |
|---|---|---|---|
| a room per agent | `agent-matrix/src/bridge.rs` `ensure_agent` | registers `@agent-<name>`, makes a direct room with the owner, stores it in `state.dms` | created, never retired |
| a Space per project | `ensure_project` | Space + `general` + `feed`; membership follows the runtime's project list | no archived state |
| adopted Spaces | `sync_adopted` | a Space the owner made is a project; the bridge manages its rooms **as the owner** (it is not a member) | see Hazards 2 |
| who answers | `on_message`, `project_targets` | mentioned agents; else the project's lead; else nobody. A hand-made room with one agent in it: that agent | a one-member project with no lead is silent |
| deleting an agent | `agent-runtime/src/store.rs` `delete`, `runtime.rs` `delete_agent` | removes spec, runs, memory, workspace, timers, project membership | the bridge is not told. `state.dms` keeps `kevin-ishot`, `chaintest`, `ping` after their agents are gone |
| grants | `agent-runtime/src/projects.rs` `effective` | project members get `project.<name>` store, `<name>.*` topics, and are told who else is in the project | membership **is** the grant |

## The decision

### 1. Every agent has a home, and the home is a Space

Per-agent direct rooms stop being created. An agent is found in one of two places:

| home | what it is | grants |
|---|---|---|
| a **project Space** | what exists today | the project's, as today |
| the **Agents** Space | a Matrix-only Space with one room per agent that is in no project, named after the agent | none |

The Agents Space is **not a runtime project**, and that is the load-bearing choice.
Making it a project called `inbox` would be simpler (no new room kind), but
membership is the grant: every unassigned agent would share one blackboard and be
told about every other one, which is exactly what ADR-0104 says must not happen
across unrelated work. So the bridge owns this Space and the runtime never hears of it.

An agent that is in a project needs no room of its own: it is in that project's rooms.
One in several projects is in each of them; where its unprompted results go is decided
in section 4.

**A new agent starts in the Agents Space** and is moved into a project on purpose.
`agent_save` takes an optional `project`; when it is given, the approval poll says
"and add it to the project `<name>`", because joining a project is a grant (its store,
its topics) and the owner approving the poll is what authorises it. Nothing is
granted by default.

holon, the agent that manages the others, keeps its own room. It is the entry point,
not a member of the fleet it manages, and it is the one place a command is typed.

### 2. Routing

Unchanged, with one added rule: **a project with exactly one member and no lead is
answered by that member.** A one-agent Space is then what a direct room was, with no
special case in the router. A room in the Agents Space has one agent in it and so
already routes by the existing hand-made-room rule.

**A project with one agent has one room**, not `general` plus `feed`: you talk to the
agent and it reports in the same place, since there is no one else to keep the report
apart from. It gets a `feed` when a second agent joins (the reconcile pass creates it
then) and a result goes to `feed` if the project has one, else to `general`.

Existing direct rooms keep working and keep being routed, so nothing is migrated under
the owner. New ones are simply not made; the old ones are removed by hand or by the
retirement in section 3 when their agent goes.

### 3. Deleting an agent: the Matrix side retires, it does not purge

Detection: an agent in `state.dms` (or in an Agents-Space room) that the runtime no
longer lists, **on three consecutive reconcile passes**. Then, for each room it was in:

1. the agent posts one line, "`<name>` was deleted", so the history ends visibly;
2. its user's display name becomes `<name> (deleted)`;
3. its user leaves the room (project rooms: it is already kicked by the existing sync);
4. a room that belonged to that agent alone (its Agents-Space room, a legacy direct
   room) is left and forgotten by the owner and dropped from the bridge state.

The Matrix **user is kept**. Old messages (coach's feed posts) name it, a deactivated
user can lose its profile in history, and a localpart can never be registered again
once deactivated, so a deleted-then-recreated `kevin-ishot` would be impossible. A
dormant user costs nothing. Re-creation reuses the user (registration must treat
"already registered" as success; to verify) and gets a fresh room.

**Safety rule.** Retirement is the reversible reaction, and it is also refused when
the disappearance looks like a runtime problem and not a deletion: if more than half
of the known agents vanish in one pass (a restart against the wrong `--state-dir`, an
empty registry), the bridge retires nothing and says so in the control room. A purge
(deactivate the user, delete the room) is a separate explicit command and not an
automatic reaction.

### 4. A schedule belongs to an agent *and* a project

An agent can be in several projects, so "where does this result go" cannot be a
property of the agent. A schedule trigger takes an optional `project`:

```json
{"kind": "schedule", "cron": "0 12 * * *", "prompt": "...", "project": "rowing", "notify": true}
```

- **With `project`**: the result is posted in that project's `feed` room (its only room,
  for a one-agent project), and the run is told it is working for that project. The same
  agent can hold one schedule per project; `0 12 * * *` for `rowing` and for `nutrition`
  are two triggers with two destinations.
- **Without `project`**: the result goes to the agent's own room in the Agents Space. This
  replaces today's "the first project by name", which was a guess.
- **Validation**: `project` must be one the agent is a member of, checked when the trigger
  is saved. If the agent is later removed from that project the trigger does not fire:
  the run is logged as `dropped` with the reason, and the next reconcile tells the control
  room. An archived project's agents are paused anyway (section 5).
- **Reminders** (`schedule_self`) go back to where they were asked for: the timer records
  the project of the run that created it, or none if it was asked for in an agent's own
  room. This needs the run to know which project woke it, which today only the bridge's
  `why` text says; the runtime's run request gains an optional `project`.

What "told it is working for that project" means is deliberately small: the project is
named in the task's `Trigger:` line and the run's store and topic grants are the agent's
as today. Narrowing a scheduled run to only that project's grants is a stronger idea and a
separate decision.

### 5. Archiving a project

Matrix has no archive. The mechanism is: **the owner leaves and forgets the Space and
every room in it**; the server keeps the history. It is what Element does for "delete
this chat" and it takes the project out of the room list and the Spaces bar. Chosen
over nesting the Space under an "Archive" Space (which hides it only if Element
collapses subspaces the way we believe it does) because it is unambiguous and the
owner asked for it. Cleanup of anything left is by hand.

Archiving has runtime consequences, and these are the part that is easy to forget:

- **The project's agents are paused**, otherwise their schedules keep running and
  posting into rooms nobody can see, and keep spending tokens. An agent is paused only
  if this was its **last active home**; one still in another live project, or in the
  Agents Space, keeps running.
- The runtime records which agents the archive paused (`paused_by_archive`), so an
  unarchive resumes **those** and never one the owner paused deliberately.
- `!project archive <name>` and `!project unarchive <name>`; `!project list` marks
  archived ones, and archiving is refused for an agent-less adopted Space (Hazards 2).
- The bridge stops reconciling an archived project (it must not re-invite the owner),
  and unarchive re-invites the owner to the Space and its rooms, then resumes the
  paused agents.

State: `ProjectInfo` gains `archived: bool` and `paused_by_archive: Vec<String>`,
both `#[serde(default)]`, in the **runtime**, because pausing is the runtime's
business and a bridge restart must not forget it. The Matrix leave is the bridge's.

## Hazards

1. **Membership is the grant.** The reason for section 1's Matrix-only Agents Space.
   Any later "put every agent somewhere" shortcut that goes through a runtime project
   reopens it.
2. **Adopted Spaces cannot be re-entered by the bridge.** It manages them with the
   owner's own session. If the owner leaves one, nobody holds a session that can invite
   them back. The way back is a member (an agent's user, which the appservice can act
   as) inviting the owner, or Synapse's admin join API. Both are untested, so the rule
   until verified is: an adopted Space is archivable only if one agent is a member.
3. **Deletion is detected by absence.** Absence also means a wrong state directory or
   an unreachable runtime. Hence three passes, the "more than half" rule, and retire-
   not-purge.
4. **One owner.** Nothing here helps a second human; the Agents Space and every archive
   assume the owner is the only person who sees the rooms.
5. **Element behaviour is assumed in four places** (Verify, below).

## Decided in review

1. A one-agent project has one room (section 2).
2. A new agent starts in the Agents Space; `agent_save` may name a project, and the
   approval poll says so (section 1).
3. A schedule carries its project (section 4).
4. **Soft delete in the runtime is wanted.** Deleting an agent moves its spec, runs and
   memory to `trash/<name>-<time>/` for 30 days and a purge removes them early. It is
   independent of the rest of this ADR and is its own slice (below). When it lands, the
   words **PERMANENTLY** in the `agent_delete` description and in the Matrix approval
   line (#330) become "recoverable until <date>", and a restore is a tool and a command.

## Still open

1. Should multi-agent projects also drop `feed` and become one room? A feed exists so
   that reports do not bury a conversation; with one human and a lead that may not hold.
   Decide after living with the one-agent case.
2. How a restored agent's Matrix side comes back: it was retired (section 3), so its user
   is reused and it gets a new room. Whether its old room is also reused is unsettled.

## Verify before building

Each is one scenario in `agent-matrix/tests/e2e.rs`, which already runs a real Synapse:

1. A room created as a child of a Space with no `is_direct` shows in the Space and not
   under People in Element (manual, one look).
2. The owner can leave and forget a Space and its rooms, and `invite` as a member agent
   brings them back.
3. Synapse's admin join API puts the owner into an invite-only room the owner left.
4. Registering `@agent-x` twice succeeds the second time; a retired user can be
   re-invited into a new room.

## Build order

0. **Soft delete** (runtime only), with the wording change above. Independent; worth doing
   first because it removes the one irreversible step in today's flow.
1. **Retirement on delete** (section 3). Stands alone, fixes today's orphans, no new
   rooms. Needs verify 4.
2. **Agents Space and the one-member rule** (sections 1, 2). Stops creating direct
   rooms. Existing ones keep working.
3. **Schedules carry a project** (section 4): the trigger field and its validation, the
   run's `project`, and delivery by it. Needs slice 2 for the Agents-Space fallback.
4. **Archive and unarchive** (section 5): runtime fields and pause, bridge leave and
   re-invite, the two commands. Needs verify 2 and 3.
5. **Cleanup command** for legacy direct rooms, if doing it by hand turns out to be
   tedious.

## What this does not do

It does not change what an agent is, how it is run, or the project model. It does not
add a second owner, soft-delete, or a purge. It does not move anything the owner
already has.
