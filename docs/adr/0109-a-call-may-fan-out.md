# ADR-0109 — A call may fan out

*A turn that asks five things at once wants one answer, not five, and wants all five
stopped when it gives up.*

**Status: accepted, and built.** `holon:park@0.3.0`
([`wit/park/park.wit`](../../wit/park/park.wit)); the join is
[`crates/holon-park/src/group.rs`](../../crates/holon-park/src/group.rs). Extends
[ADR-0100](0100-a-parked-turn-is-a-record-not-a-held-thread.md) and
[ADR-0106](0106-a-resumed-turn-knows-what-it-may-repeat.md); breaking against 0.2.0, as
*Consequences* lists.

## The problem

A park ticket is one outstanding call. A turn that fans out — five sub-agents, five checks, a
batch of lookups — parks five of them, and today gets five tickets, five `PARK_WAKE`
announcements and five deliveries, and is left to count them itself: which have answered, whether
it is time to go on, what to do with the others when one has failed. And when it gives up, it
cancels five tickets one at a time, with a window between each in which a late answer lands on a
call nobody wants any more.

Two things in that are the platform's to own, because they are about *state that must survive a
crash*: "have all of these settled" is a fact about five records, and "stop all of these" is five
writes. A caller that dies between the second and the third leaves the rest running.

## What was read

[pi-durable](https://github.com/earendil-works/pi/tree/main/packages/durable) gives every task
an owner — a conversation or another task — lets a task wait on its children "with `allSettled`
or `failFast`", and propagates an abort bottom-up, "children are aborted first, so each task
undoes its own effects". Those three — ownership, the two joins, and a cascade that goes
children first — are what is taken. What was read is its README, not its source; and its tasks
are steps of one process's state machine, where these are outstanding calls on a fleet, so the
mechanism is this repository's own.

## The decision

**A group is a call resolved by its children.** Parked with `group: {policy, children}`: how it
joins, and how many children it will have. A child is parked with `parent`, the group's
`correlation` in the same session. The group is parked first.

| policy | resolves when |
|---|---|
| `all-settled` | all `children` have been parked and have settled, however each ended |
| `fail-fast` | one child fails (an `ok: false` answer, an interruption, a cancellation) — or all settle |

The table is one pure function, `group::join_decision`, tested as a table. "All parked" is part
of it: a group declared with three children is not complete when the first settles before the
other two exist.

**It resolves as `joined`.** `take-ready` on a group returns every child in the order it was
parked, how each ended, why the group stopped (`all-answered`, `some-failed`, `failed-fast`,
`timed-out`), and `missing`, the children declared and never parked. `resolution` gains the case
and a consumer must match it; a group cannot be read as a failed answer.

**A child is consumed by its group.** It is settled by its own `wake`, `interrupt`, `cancel` or
recovery, and then it is not announced, not delivered and not taken on its own: the group is what
a turn waits on, and what `PARK_WAKE` is told. When a group resolves its settled children are
closed (`resumed`), so none lingers as an answer nobody will take. `take-ready` on a child is
`invalid`: "take the group".

**Bottom-up, always.** A group never resolves with work still running. When it resolves because
a child failed under `fail-fast`, or because its own deadline passed, the children still parked
are cancelled *first*, and only then is the group written. `cancel` of a group cancels its
children first, then the group. So a crash can leave a group unresolved with some children
already cancelled, which the next join finishes, and never a resolved or cancelled group with a
child still running that nothing will cancel. A child whose answer lands while the join is
deciding keeps it: the cancel is conditional on the child still being parked.

**The join runs wherever a child can change, and again when that is retried.** After `wake`,
`interrupt`, `cancel` and each move of a recovery pass on a child, and also after a retry of
those that finds the child already moved — which is what a crash between the child's write and
the group's leaves. A caller that never retries is covered by the recovery pass, which settles
every group, finishes a resolved or cancelled group's children (a pass that died between the two
writes), and prefers the join's own answer to `timed-out` when both are true.

**A group's `deadline` is its backstop.** Without one a group waits as long as its children do.
With one, a group still waiting at it resolves `timed-out` with what the children had done.
`replay` on a group is ignored: each child has its own and is recovered on its own, and the join
sees what recovery did to them.

**The engine says what to announce.** `wake`, `interrupt` and `cancel_announcing` return the
messages to put on `PARK_WAKE`: the ticket itself if it is not a child and this call moved it, or
the group this call resolved, never a child. `comp-park` publishes exactly that, and the
recovery pass's re-announce row skips children, so a down sink is not told about five tickets it
will never be handed.

**The places in a group are counted atomically.** The group's child list is bounded by what it
was declared with and the bound is checked in the same compare-and-set that adds a child, so
children racing for the last place cannot both have it. The place is taken *before* the child's
record is written, so a crash between leaves a place held for a ticket the caller's retry will
create; if nobody retries, the group's deadline resolves it, `missing` counting the ticket that
never came.

**Refused, all `invalid`:** a child whose group does not exist, is not a group, is another
session's, or has already resolved; a child past the declared number; a group that is also a
child; `children` of 0 or over 1024; `wake` or `interrupt` on a group; a child re-parked under
another parent.

## What this does not do

- **Groups do not nest.** One level: a group of calls, not a group of groups. A sub-agent that
  fans out again does so in its own session with its own group, which is how its parent sees it
  — as one child that eventually answers. Nesting would need a recursive result type, which WIT
  does not have, and a cycle check; neither earns its keep before something needs depth.
- **A cancelled child is given up on, not stopped.** The contract has never dispatched
  anything, and still does not: cancelling the ticket does not reach the far side's work, and the
  answer it sends anyway is accepted and dropped. A caller that must stop remote work does that
  itself, which `comp-resume`'s delivery of the group is the moment to do.
- **No partial results.** A group is read when it resolves. A turn that wants each child's answer
  as it arrives wants children that are not in a group.
- **A group cannot grow.** Its size is declared; a fan-out whose width is not known when it
  starts parks its calls as plain calls, or parks a group at each step.
- **The `PARK_WAKE` announcement of a group is not the durability boundary**, as for any ticket:
  a group resolved and never announced is found by the re-announce row.

## What found what

The crash matrix ([`tests/crash.rs`](../../crates/holon-park/tests/crash.rs)) gained five cases —
registering a child, the last child's answer, a failure under fail-fast, a cancelled group, a
group's timeout — which, in both crash modes and both retry orders, are 80 of its 132 crash
points, over memory and over real JetStream, with invariants for groups added (a child is listed
under its group; nothing a group lists is missing; a resolved or cancelled group has no child
still open; a parked group's join does not already say "resolve"). They found **nothing in the
engine**: the design was built to make every step idempotent and every retry heal, and the
matrix confirms it. (Two points failed on the harness's own expectation at first: a retried
`interrupt` of a child its group had already consumed is told `already-closed`, which is the
contract.) Mutation checks confirm the matrix can fail: a retry that no longer runs the
join fails 28 points, a resolution that no longer closes the children fails 32, and a fail-fast
that no longer cancels first fails 8.

What did find things were real-JetStream tests:

- **Order.** The first live fan-out returned children in the order of their ticket hash. They
  had been sorted by `parked_at`, and children registered in one millisecond tie. A group's
  children are now read in the order of its child list, which is only appended to — the order
  they were parked.
- **Contention.** The loop that grows a list gave up after 8 lost compare-and-sets, which was
  sized for a session's own few concurrent parks. A fan-out registers its children at once.
  Measured, with no retry by the caller: **16 racers, 6 turned away; 64 racers, 51; 256
  racers, 241** (children registered: 10, 13, 15). With 64 attempts and a few milliseconds of
  backoff per lost race: all 16, all 64 and all 256 registered, the last in 0.77 s. The loop is
  shared with a session's list, which gets the same bound.

## Consequences

- **`holon:park` is 0.3.0.** `resolution` gains `joined` (a consumer that matches exhaustively
  must handle it), `outbound-call` gains `parent` and `group`, `ticket-entry` gains `parent` and
  `group`. Records and messages written by 0.2.0 still read: every new field defaults to "not a
  group, not a child".
- **A turn sees a group's children in `pending` and `oplog`**, with `parent` set, and may filter
  them out; the resumer never delivers them.
- **`comp-resume` delivers a `joined` event** (`{"event": "joined", "joined": {…}}`), keyed
  `<group>:joined`, and treats a leftover message that names a child as stale.

## Proof

[`tests/group.rs`](../../crates/holon-park/tests/group.rs) against memory: registration and every
refusal; both joins and the outcome of each row of the table; the declared size and a group not
resolving early; fail-fast cancelling before it resolves and a late answer dropped; a failure of
any kind tripping it; the deadline, and the join's answer beating a deadline that also passed;
cancel cascading children-first and a resolved group still cancellable; eight children answering
at once resolving the group exactly once, twenty rounds; twelve racing for three places taking
exactly three; delivery as one `joined` event and a child message being stale; the sweep
announcing the group and never the child that tipped it; repair listing a child a crash left out.
Over real JetStream ([`tests/live.rs`](../../crates/holon-park/tests/live.rs)): a fan-out
resolved once and delivered as one event in parking order, 24 racers for 5 places, fail-fast.
Through the real components, daemon and resumer
([`reconciler/tests/e2e_park.rs`](../../reconciler/tests/e2e_park.rs), `bash e2e/park.sh`): a
fan-out delivered as one `joined`, fail-fast delivering with the siblings cancelled, and every
refusal over the wire.

## Alternatives

- **Deliver every child and let the turn count.** The status quo. Rejected: it is the problem.
- **Resolve a group by a count of answers instead of a declared size.** Rejected: "N answered"
  cannot tell a group still being registered from a group that is done, and the first child to
  settle before the last is parked would resolve it.
- **Cancel the group first, then its children.** Rejected: a crash between leaves a cancelled
  group with children running that nothing is going to cancel. Children first leaves a live
  group, which a retry or a pass finishes.
- **Put `joined` in `call-result`'s `body` as JSON.** Rejected: an answer whose meaning depends
  on parsing bytes is how a consumer reads a group as a failed call.
- **A recursive tree.** Not now, above.
