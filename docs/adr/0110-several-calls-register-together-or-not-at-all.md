# ADR-0110 — Several calls register together, or not at all

*Registering five calls one at a time has a middle. A commit has a before and an after, and the
middle belongs to the platform, not the caller.*

**Status: accepted, and built.** `commit` in `holon:park@0.4.0`
([`wit/park/park.wit`](../../wit/park/park.wit)); the engine is
[`crates/holon-park/src/batch.rs`](../../crates/holon-park/src/batch.rs); the sweep that finishes a
dead caller's commit is `comp-resume`'s. Extends [ADR-0109](0109-a-call-may-fan-out.md): a fan-out
is the first thing it is for.

## The problem

A fan-out is N+1 parks, and each park is several writes. Called one at a time, that has two middles:

- **A bad effect.** The third call of five is refused (another session owns its correlation, its
  group does not exist). The first two are already parked, and the caller has an error for one call
  and a mess for two.
- **A dead caller.** It dies after the group and the second of five children. A group declared with
  five waits for three that will never be registered, until its deadline, if it has one.

The caller can repair both by being careful and by retrying; the second only if it comes back. What
cannot be repaired from outside is the first, and what is wasteful to repair from outside is the
second.

## What was read

[pi-durable](https://github.com/earendil-works/pi/tree/main/packages/durable): "A commit is atomic.
It can append entries, edit documents, and create tasks together, so either all of it is stored or
none of it", nothing is shown to a client before its commit is stored, and "this atomicity covers
only Pi Durable's storage, not remote APIs". The unit is right. What it rests on — one store, one
process — is not what is here, where there is no transaction across JetStream keys; so the mechanism
is a **write-ahead intent**, the technique `holon-vcs` already uses for its operations (ADR-0099):
write down what is about to happen as one record, then do it idempotently, and let anyone who finds
it unfinished finish it. What was read is its README, not its source.

## The decision

**A commit is a list of effects.** `park`, `wake`, `interrupt`, `cancel`, each what the call of the
same name does, naming by `correlation` a call that already exists. It returns its id and what each
park said, in order.

**Admission is all or nothing.** Every effect is checked before anything is written, and a list with
one bad effect registers none of them. The checks are `park`'s own, applied to the list as a whole:
no correlation parked twice, a child's group parked earlier in the list or already there and open, no
group given more children than it was declared with (counting the ones already there), no
correlation another session owns, and every `wake`, `interrupt` and `cancel` naming a call the list
parks or one that exists, none `wake`-ing or `interrupt`-ing a group.

**Then the intent, as one record.** The whole list is written as a single record, `batch.<id>` in the
same KV bucket, state `pending`. The id is a hash of the session and the effects in order, so the
identical commit again is the same batch, and a retry cannot make a second one. From the moment that
record exists, the effects will all be applied or all be undone — by the caller, or, if it dies, by
whoever next finds it.

**Parks first.** A `park` is the one effect that can still be refused once the intent exists, since
another session may take a correlation between the check and the write, and the one that can be
undone: cancel what it made. `wake`, `interrupt` and `cancel` cannot be undone, so none is applied
until every `park` has succeeded, and each is idempotent (a call already closed is the effect
already done). A commit therefore never has an irreversible effect behind it when it has to roll
back.

**A rollback cancels what the commit made, and only that.** Each ticket a commit creates is stamped
with the batch id in the same write that creates it (`Record.batch`), so there is no window in which
a ticket exists that its commit does not know it made. Abort walks the list last-first, so a group's
children go before the group, and cancels the stamped tickets. A call that was already parked before
the commit, and merely named by it, is untouched. The commit ends `aborted` with the reason, and
reports `invalid`.

**A pass finishes what a dead caller left.** `Engine::complete_batches`, run by `comp-resume`'s sweep
after its repair and before its recovery, drives every commit still `pending` after `--batch-lease-secs`
(30 s by default): one younger is presumed to be in the hands of the caller that wrote it. Driving is
idempotent, so two drivers meeting on one commit is wasted work and not a fault; the lease only
keeps it rare. What the finished effects resolve — a group, say — is announced.

**What it announces is the engine's say.** A commit returns what to put on `PARK_WAKE`, as `wake` and
`interrupt` do: the group a child's effect resolved, never the child.

## What this does not do

- **No isolation.** While a commit is pending a reader may see some of its tickets and not others.
  It is atomic in what lasts — it ends done or aborted, whoever drives it — and not in what is seen
  on the way. Nothing was hidden to begin with: park has never had read-your-own-uncommitted-writes
  to protect.
- **Park's own records only.** A commit cannot include a write to another store: an agent's
  transcript is files under `comp-agentd`'s `--state-dir`, a session document would be somewhere
  else, and nothing here can make two stores commit together. The honest shape for that is the
  same intent turned the other way: the parked call is written first, in JetStream, and what the
  agent keeps is derived from it. That is a change to the agent, not to this crate.
- **A finished commit registers calls nobody dispatched.** Park has never sent anything, and a commit
  finished by a pass for a caller that is gone has parked calls that caller will not send. They are
  handled as any parked call nobody answers is — by their deadlines — which is why a commit is no
  substitute for setting one.
- **A commit that is aborted stays aborted.** The identical list again is refused, not retried;
  usually it is refused earlier, by the check that sees the correlation taken. A caller that wants to
  try again changes what it asks.
- **At most 256 effects.** The intent is one record, and it is read whole.

## What found what

The crash matrix ([`tests/crash.rs`](../../crates/holon-park/tests/crash.rs)) gained three cases —
a commit of a fan-out, a commit that replaces one call with another, and a pending commit that can
never be applied, found by a pass — which, in both crash modes and both retry orders, are 104 of its
236 crash points over memory and over real JetStream, with invariants added (a ticket a commit made
has its intent behind it; no commit is left pending; a done commit lost no park; an aborted commit
left nothing of its own open). They found **nothing in the engine** on their first run. Mutation
checks say they, and the memory tests beside them, can fail: an abort that no longer cancels what the
commit parked fails three tests, one that cancels whatever it names fails one, and effects applied in
the order listed rather than parks first fails one.

## Consequences

- **`holon:park` is 0.4.0.** A function and new types: a 0.3.0 caller is unaffected, and the version
  moves because the shape did (`wit/SURFACES.md`).
- **`Record.batch`, `batch.<id>` in the bucket, `RecoveryPolicy.batch_lease_ms`, and
  `comp-resume --batch-lease-secs`.** Records from before read as made by no commit.
- **`ParkStore` grows four methods** (`create_batch`, `get_batch`, `cas_batch`, `list_batches`).
  A recovery pass reads every batch key, like it reads every ticket (the same `ponytail:` limit
  applies to both).

## Proof

[`tests/batch.rs`](../../crates/holon-park/tests/batch.rs) against memory: a fan-out as one unit and
the same list again found done; a commit being its effects in order; a list with one bad effect
registering none of eleven kinds of badness, and writing no intent; a group not over-filled by a
commit, counting what is already there; replacing one call with another; a child's group resolved by
a commit's effects and announced as the group; a call already closed being the effect done; a park
refused after the intent was written rolling back what the commit parked, leaving a call that was
already there alone, and never applying the cancel listed first; a rolled-back group cancelled
children first; a dead caller's commit finished by a pass once the lease has run out and not before;
a pass rolling back a commit that can never be applied; and the sweep announcing what a finished
commit resolved. Over real JetStream ([`tests/live.rs`](../../crates/holon-park/tests/live.rs)): a
dead caller's intent finished by the resumer's own sweep, and six callers committing the identical
list at once making one commit and four tickets. Through the real components, daemon and resumer
([`reconciler/tests/e2e_park.rs`](../../reconciler/tests/e2e_park.rs)): a fan-out committed, the same
commit again, a bad list and an empty one refused with nothing left behind, and a second commit
whose `wake` effects resolve the group, which `comp-park` announces and `comp-resume` delivers once.

## Alternatives

- **Leave it to the caller's retry.** The status quo. Right when the caller comes back, and it is
  still what finishes most commits; what it cannot give is all-or-nothing admission, or a fan-out
  that completes when the caller does not return.
- **JetStream's atomic batch publish.** Newer servers can, I understand, apply several publishes to
  one stream atomically, and a KV bucket is a stream. Not used, and not verified here: I did not test
  which server versions have it. `infra/compose.yaml` pins `nats:2.10`, and a contract that depends
  on a server newer than the one the repo deploys is one the repo cannot keep. Not ruled out for a
  deployment that controls its server.
- **Roll back every kind of effect.** Rejected: `wake`, `interrupt` and `cancel` have no undo. Parks
  first, with the rest behind them, is what makes a rollback possible at all.
- **Record progress in the intent after each effect.** Rejected: it costs a compare-and-set per
  effect and leaves a window between an effect and the record of it, which is exactly the window the
  stamp on the ticket does not have.
- **Hide the pending commit's tickets from readers.** Rejected for now: it would need a visibility
  flag on every read path, for an isolation nothing has asked for.
