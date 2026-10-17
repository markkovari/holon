# ADR-0106 — A resumed turn knows what it may repeat

*A call parked before a crash comes back in doubt. Whether anything may send it
again is a fact about the call, and only the caller knows it.*

**Status: accepted, and built.** The contract is `holon:park@0.2.0`
([`wit/park/park.wit`](../../wit/park/park.wit)); the policy is
[`crates/holon-park/src/recover.rs`](../../crates/holon-park/src/recover.rs); the
resumer ADR-0100 left unbuilt is `comp-resume`
([`reconciler/src/bin/resume.rs`](../../reconciler/src/bin/resume.rs)). Amends
[ADR-0100](0100-a-parked-turn-is-a-record-not-a-held-thread.md): the contract it
named is now 0.2.0, and the resumer it said was "somebody else's code" exists.
A crash-injection matrix (*Proof*) found and closed a hole in ADR-0100's `park`,
described under *What a crash used to leave*. **Not built:** anything that parks on
behalf of `comp-agentd` — see *What this does not do*.

## The problem

ADR-0100 made a call's *existence* durable. It is written down before it is
dispatched and closed after its answer is taken, and "a crash between 'sent' and
'heard back' must read as 'still outstanding', never as 'never happened' or
'done'." It did not say what a restarted turn should *do* about an outstanding
call, and the answer is not the same for every call:

- **Send it again.** Right for a read, or for a write the far side deduplicates.
  Wrong for a payment: the card is charged twice.
- **Tell the turn it was interrupted, and send nothing.** The model, or a
  person, can look at the far side and reconcile. Right for the payment.
- **Drop it.** Right for a call whose answer a restarted turn has no use for.

Today `park` answers `already-parked` and the caller guesses. Nothing acts on a
ticket whose answer never comes either: `expired` is a label `pending` applies
when it is read, and nothing reads it unprompted. And the resumer that was to
deliver an answer to the turn that wanted it was named, drawn, and not written.

## What was read

[pi-durable](https://github.com/earendil-works/pi/tree/main/packages/durable)
(Earendil's durable agent harness) meets the same fork with one declaration: a
tool says `replay: 'safe'` or, by default, does not. A safe call interrupted by a
crash is run again on reopen; an unsafe one is *not* — the model is handed an
`interrupted` result instead. Its own write-up is plain that request-id
deduplication "does not make external actions happen exactly once". What was read
is its README and one write-up, not its source.

The declaration is worth taking and the architecture is not: pi-durable is one
process owning one store, and a parked call here outlives the process that made
it, on a fleet, behind JetStream. So the *vocabulary* (`safe`, `unsafe`,
`interrupted`) comes across, and the mechanism is this repository's own.

## The decision

**1. A call says how it may be replayed.** `outbound-call` gains `replay`, a
required field in the WIT and `unsafe` when a JSON caller leaves it out:

| `replay` | sending it again | past its deadline |
|---|---|---|
| `safe` | harmless: a read, or a write deduplicated on `correlation` | re-dispatched, at most `max-redispatches` times, then treated as `unsafe` |
| `unsafe` (default) | might repeat an effect; nothing here ever does it | `interrupted` |
| `abandon` | pointless: a restarted turn has no use for the answer | cancelled, and nobody is told |

**2. Interrupted is an outcome, not an error.** A new status, `interrupted`, is
taken exactly like `ready`; `take-ready` now returns

```
variant resolution { answered(call-result), interrupted(interruption) }
record  interruption { reason: string, at: u64, by: agent }
```

so a consumer cannot read an interruption as a failed answer without writing the
`match` that ignores it. `interrupt(ticket, reason, by)` is the new operation:
`parked` to `interrupted`, idempotent (the first reason stands), a no-op on a
ticket whose answer landed first (the answer is then what is taken), and a late
answer to an interrupted ticket is accepted and dropped, as one to a cancelled
ticket is. The `reason` is what the turn is told. The pass writes it as "no
answer arrived by the deadline, and this call is not replay-safe: it may or may
not have taken effect. Check on the far side before repeating it."

**3. A deadline is how a call asks to be given up on.** A call with none waits
forever and no pass ever touches it. This is deliberate and has to be said to
callers: set a deadline on a call you want a crash to be reported on.

**4. The table is one pure function.** `recover::plan(record, now)` returns the
one move a pass would make, and `Engine::recover` makes it with one
compare-and-set per ticket and no retry:

| ticket | `replay` | | the pass |
|---|---|---|---|
| `parked`, no deadline or not past it | any | | nothing |
| `parked`, past deadline + `deadline-grace` | `unsafe` | | `interrupted` |
| | `abandon` | | `cancelled` |
| | `safe`, redispatches left | | `redispatches + 1`, deadline pushed out by `retry-after` |
| | `safe`, none left | | `interrupted` ("after N re-dispatches") |
| `ready` or `interrupted` | — | untaken for `reannounce-after` | announced again, at most once per gap |

The grace exists because a pass acts irreversibly and a deadline is a wall-clock
instant written by whoever parked the call, on a box whose clock may disagree.
A lost compare-and-set means another pass or a `wake` got there first; the next
pass reads fresh. Several resumers sweeping at once therefore grant each
re-dispatch and each interruption exactly once between them.

**5. A restarted turn reads `park`'s answer against `replay`.** Re-running its
own park-then-dispatch sequence is the caller's recovery path, and the contract
now says what each outcome means:

| `park` says | `safe` | `unsafe` | `abandon` |
|---|---|---|---|
| `parked` | dispatch | dispatch | dispatch |
| `already-parked` | dispatch again | do not; wait, or `interrupt` | `cancel` |
| `already-woken` | `take-ready` | `take-ready` | `take-ready` |

`already-woken` covers an interrupted ticket: there is nothing to dispatch, and
`take-ready` says why.

**6. A resumer, `comp-resume`, delivers and recovers.** Two loops in one native
process, over the same KV bucket as `comp-park` and sharing nothing else with it:

- **Deliver.** A durable pull consumer on `PARK_WAKE`. For each message it reads
  the ticket **without consuming it** (`Engine::resolution`), `POST`s what it
  holds to `--sink-url`, and only when the sink accepts takes the ticket and
  acknowledges the message. At least once: a resumer killed after the sink
  accepted and before the ticket closed is replaced, the server redelivers, and
  the sink is handed the same delivery under the same `Idempotency-Key`
  (`<ticket>:answered`, `<ticket>:interrupted`, `<ticket>:redispatch:<n>`). A
  message is a nudge, never the truth: what is done is decided by the ticket as
  it is now, so a `redispatch` for an attempt the ticket has moved past is
  finished without a call.
- **Recover.** Every `--sweep-secs`, `recover` as above, then announce what moved
  on `PARK_WAKE` so the deliver loop picks it up.

The sink is whatever drives the turn. The contract is one `POST` of
`holon_park::resume::Delivery` — `event` is `answered`, `interrupted` or
`redispatch` — answered `2xx` (accepted), `410` (nobody is there; the ticket is
cancelled, since retrying would never end) or anything else (not now; the message
returns after 1 s, doubling to 256 s). Re-dispatch is a *notification*, not an
action: the ticket holds a description of the call and never its payload, so the
turn, which owns the payload, sends it again.

## What this does not do

- **No dispatch tracking.** A `dispatched` mark written after sending was
  considered and rejected: it can prove a call *was* sent and never that it was
  *not*, because the crash window only moves to between the send and the mark. So
  it could help `safe` calls, where it does not matter, and not `unsafe` ones,
  where it would.
- **Not exactly once.** An unsafe call is never re-sent by anything here, but a
  call whose send did not complete, or whose caller never re-sent it, is also
  never reported unless it has a deadline. What the far side did is the caller's
  to reconcile, which is what `interrupted` says.
- **A late answer to an interrupted call is dropped.** A turn told "interrupted"
  and then, later, handed the real answer is a second delivery with its own
  idempotency story. Not built.
- **`comp-agentd` parks nothing.** Its tools run in-process, its crash handling
  fails the running task, and the one wait it could park — a human's approval —
  lives in a channel in memory. Wiring that is a change to agentd's loop
  (checkpointing the conversation per step, not at the end of a task) and its own
  ADR. This ADR delivers the contract such a turn would use and a resumer that
  serves it.
- **`comp-resume` has no sink to deliver to yet.** `apps/park.toml` runs it, as a
  *worker*: a `[[daemon]]` with no `addr`, which [`cli/src/selfhost.rs`](../../cli/src/selfhost.rs)
  renders without `--addr` and without a `<name>-url` to agree with, because it listens on
  nothing ([SELFHOST.md](../SELFHOST.md)). `--sink-url` is required and nothing in this
  repository serves it, since `comp-agentd` parks nothing (the next bullet), so the spec points
  at where `comp-agentd` would serve `/park/deliver`. Until something answers there a delivery
  is retried with backoff and the ticket stays `ready`, which a turn that polls `pending` still
  finds; the recovery pass, which needs no sink, runs regardless. Run by hand from the rendered
  units: an answered call delivered, and an overdue one interrupted by the pass.
- **A recovery pass reads every ticket.** `recover` lists the bucket's keys and
  reads each record, and nothing deletes a ticket, so a pass costs what the bucket
  holds. Fine for thousands; an index of open tickets, or of deadlines, is the
  change if it ever holds millions (marked in `store.rs`).
- **The announcement is not the durability boundary**, as `PARK_WAKE` never was.
  If `comp-park`'s publish, or a pass's, fails, the ticket has still moved and the
  re-announce row finds it. A lost *re-dispatch* announcement costs that attempt,
  and attempts are bounded.

## What survives what

| dies | between | state left | what happens |
|---|---|---|---|
| `comp-park` | the answer is recorded and announced | `ready`, no nudge | a pass announces it again after `reannounce-after` |
| `comp-resume` | the sink accepted and the ticket closed | `ready` / `interrupted`, message unacked | redelivered after `ack-wait`; the sink dedupes on the key |
| `comp-resume` | a pass moved a ticket and announced it | `interrupted`, no nudge | the next pass announces it again |
| the sink | — | message nak'd | retried with backoff; the ticket stays where it is |
| two resumers | a pass | — | compare-and-set picks one per ticket; a delivered twice is absorbed by the key |
| NATS | anything | whatever JetStream stored | everything waits; nothing is invented |

### What survives `kill -9` of NATS itself, and what does not

Every row above kills a client of JetStream. The server is the part that actually
holds the tickets, and there are three different ways to lose it:

| NATS is lost by | what is lost | how it is known |
|---|---|---|
| the **process** dying (`kill -9`, OOM, a crash) | **nothing acknowledged** | measured: [`tests/restart.rs`](../../crates/holon-park/tests/restart.rs) parks, wakes, interrupts, cancels and takes thirty tickets across three sessions, SIGKILLs a real `nats-server` the moment the last write is acknowledged, restarts it on the same directory, and finds every ticket in the state it was acknowledged in, each reachable by session and by correlation, and exactly the announcements nobody had taken still queued on `PARK_WAKE`, once each |
| the **host** losing power, or the OS crashing | writes acknowledged since the last `fsync`: up to `sync_interval` of them | **documented, not measured**: JetStream "does not immediately `fsync` data to disk"; the default `sync_interval` is 2 minutes and "the data will be `fsync`-ed no later than this interval" ([nats.docs #896](https://github.com/nats-io/nats.docs/pull/896/files), [`sync_interval`](https://docs.nats.io/reference/config/jetstream/sync_interval)). `sync_interval: always` makes "an explicit sync … occur on every write", which "will degrade the max throughput due to the additional I/O calls". `infra/compose.yaml` sets neither, so the default applies |
| the **disk** | everything, if it is the only copy | read off a live server by the same test: the ticket bucket and `PARK_WAKE` are both on file storage, by default in **one copy**. `--replicas` on `comp-park` and `comp-resume` keeps more on a cluster, and with the node holding a one-copy bucket killed every ticket is out of reach until it is back: [ADR-0111](0111-park-keeps-as-many-copies-as-it-is-told.md) measures a node killed, and does not measure a disk lost and replaced |

The first row is the claim ADR-0100 made and this repository can now check. The
second is the cost of leaving a default alone, and it bites this design in one
particular place: an answer is one acknowledged write, and the webhook that carried
it has already been told it succeeded. If a power cut takes that write the ticket is
`parked` again, the answer is gone from the world, and the only thing that will
notice is the deadline — which interrupts the call and sends the turn off to
reconcile. That is the design doing its job, and with no deadline nothing notices at
all. Nothing in `holon-park` can close the window; `sync_interval: always` on the
NATS that holds the bucket can, and is the setting to reach for on a box that can
lose power with calls outstanding.

## What a crash used to leave

`park` writes the ticket's record first and its two index entries after: its place
in its session's list, which `pending` and `oplog` read, and the claim on its
`correlation`, which a webhook's `wake` finds it by. A process that died between
left a ticket reachable only by its own id, and the retry the engine's own comment
named as the cure did not cure it: `park` on an existing ticket returned before
writing anything. Found by the crash matrix below on its first run — eight of its
fifty-two crash points failed, all in `park`, none in the ADR-0106 machinery:

- a crash after `create` (or before `index_session`): the ticket is invisible to
  `pending` and `oplog`;
- a crash after `index_session` (or before `claim_correlation`): the ticket is
  listed, but a webhook's `wake` is refused `not-found`, and a sender that does not
  retry has lost its answer.

Two changes close it. A `park` retry now writes both entries again (they are
idempotent), and answers `invalid` if another session took the correlation in the
gap. And `Engine::repair`, which the resumer's pass runs first, puts back whatever
a caller that never returned left behind — reading each session's list once and
each ticket's claim once, writing only what is missing, and reporting a correlation
two sessions both ended up with rather than choosing between them.

## Consequences

- **`holon:park` is 0.2.0 and breaks 0.1.0 twice:** `outbound-call` has a new
  required field and `take-ready` returns `resolution`. The only importers in the
  tree are `park-store` and `park-gateway`, both updated, and the wire is
  [`components/park-store/CONTRACT.md`](../../components/park-store/CONTRACT.md).
  Records already in a bucket still read: every new field defaults to "nothing has
  happened". A `PARK_WAKE` message from before this still reads as "resolved".
- **`ticket-entry` carries what a restarted turn needs to find its call** —
  `correlation`, `replay`, `deadline`, `redispatches`, `interrupted-at` — because a
  ticket id is a hash and cannot be mapped back to a journal entry.

## Proof

**The crash matrix** ([`tests/crash.rs`](../../crates/holon-park/tests/crash.rs), the
technique of `holon-vcs`'s `tests/common/crash.rs`): every store operation behind a
fuse that kills the "process" at the k-th write, for every k of nine operations —
`park`, `wake`, `interrupt`, `take-ready`, `cancel`, a pass that interrupts two
tickets, a pass that grants two re-dispatches, a re-announcement, a delivery — in two
modes, *before* the write and *after* it (the write lands and the reply is lost,
which against a networked store is as common, and is the one that duplicates), each
retried both before and after a repair. After every crash it checks that each ticket
is coherent and reachable by every index, that the operation happened exactly once,
and that the store keeps working. 52 crash points, over memory and over real
JetStream; the first run, before the fix above, failed 8.

Beyond that, against memory ([`tests/recover.rs`](../../crates/holon-park/tests/recover.rs)):
the table above row by row, with time injected; the grace boundary; a pass run
twice at one instant acting once; eight racing passes granting each of twenty
tickets exactly once; a store that refuses writes standing in for a resumer that
died after the sink accepted — redelivered with the same key, never lost. Against
real JetStream ([`tests/live.rs`](../../crates/holon-park/tests/live.rs)): the
same, plus a resumer's runtime dropped mid-delivery and replaced, an answer whose
announcement was never published found by the sweep, and a message in the
pre-0106 format. End to end
([`reconciler/tests/e2e_park.rs`](../../reconciler/tests/e2e_park.rs), through
`bash e2e/park.sh`): the real `park-gateway` ⊕ `park-store` components in
`comp-host`, a real `comp-park` and a real `comp-resume` against a private NATS,
and `comp-resume` killed with SIGKILL while its sink held the answer.

## Alternatives

- **Store the call's payload in the ticket, so park can re-dispatch itself.**
  Rejected: it would make `comp-park` know protocols and hold credentials, and
  ADR-0100's boundary is that it dispatches nothing.
- **An `interrupted` flag on `call-result`.** Rejected: every `wake` caller would
  have to set it, and a consumer that forgets to read it treats an interruption as
  a failed answer. A variant cannot be forgotten.
- **Make `take-ready` idempotent per taker, instead of reading before taking.**
  Rejected: resumers are a pool sharing one identity, and the one that retries
  after a crash is not the one that took.
- **Put the resumer in `comp-park`.** Rejected: ADR-0100 drew that line, and the
  kill test is only meaningful if the resumer can die without the store.
- **Default `replay` to `safe`.** Rejected: a caller that never thought about it
  should get the cautious reading, which costs a call one `interrupted`, not one
  charge twice.
