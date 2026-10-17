# ADR-0111 — Park keeps as many copies as it is told, and says so when it cannot

*A ticket on one node is out of reach when that node is. How many copies there are is the
deployment's to say, and the flag that says it must not be able to say it falsely.*

**Status: accepted, and built.** `--replicas` on `comp-park` and `comp-resume`;
`connect_with_replicas` in [`crates/holon-park/src/nats.rs`](../../crates/holon-park/src/nats.rs).
Closes the third row of [ADR-0106](0106-a-resumed-turn-knows-what-it-may-repeat.md)'s *What survives
`kill -9` of NATS itself, and what does not*, which said that `comp-park` had no flag for a
clustered deployment and that one would need it. No change to `holon:park`.

## The problem

Both the ticket bucket and `PARK_WAKE` were created with the client's default: one copy, on whichever
server JetStream picked. On a cluster that is a single point of failure that looks like three nodes.
The node holding the bucket dying is not the loss ADR-0106 measured (`kill -9` and a restart lose
nothing acknowledged); it is an outage. Measured on a three-node cluster, with the node that holds a
one-copy bucket killed: reading it fails (`503 no responders` from the library, a timeout from the
`nats` CLI) and cannot succeed until that node is back, when the ticket is in it. Every parked call
is out of reach, a webhook's answer is refused, and a turn cannot be resumed, for as long as one
machine is down.

## What was found

None of these was in the design before it met a server:

- **An existing bucket keeps what it was made with.** `get_key_value` and `get_or_create_stream` hand
  back what is there, whatever it was created as. A flag that only set the count on *creation*
  would do nothing to every deployment that already exists, which is every deployment, and say
  nothing about it.
- **`connect` took one address.** `async-nats` parses `--nats-url` as a single server, so a daemon
  told about one node of three cannot start while that node is down, however many copies exist of
  what it would read. A comma-separated list was not accepted at all.
- **`async-nats` accepts an empty address.** Parsed, it is a server with an empty host on port 4222,
  so `a,,b` is not refused there: it is a list with a third server in it that nobody named.
- **`nats stream edit KV_x --replicas 3 --force` fails with no terminal** (`cannot ask for
  confirmation without a terminal`) where the same command on `PARK_WAKE` works. The first
  version of the error here printed that command, and running it as printed is what showed it.
  `nats kv edit x --replicas 3` is the one for a bucket, and changes only the replica count: 21
  fields of the stream's configuration compared before and after, one different.

## The decision

**`--replicas N`, 1 to 5, default 1, on `comp-park` and `comp-resume`.** It applies to the bucket and
to `PARK_WAKE`. The stream is only a nudge — the record is the answer, and a recovery pass finds a
ticket whose announcement was lost — so losing it costs latency, not tickets. It is kept in as many
copies anyway: with one, the node holding it going down stops every announcement and delivery,
and the copies of the bucket would protect the tickets and not the service. Both daemons take it
because either may be the one that creates them.

**The number is a floor, and an existing stream is never changed.** Asked for three, a bucket or
stream that already keeps one is **refused**, naming it and the command that raises it:

```
Error: NATS at … (bucket pk, stream PK_WAKE, 3 replicas)
Caused by:
    storage: bucket `pk` is kept in 1 copy and 3 were asked for; raise it first, with
    `nats kv edit pk --replicas 3`
```

A stream with more copies than asked for is fine, so a resumer left on the default never lowers what
`comp-park` asked for and is never refused for it.

Refused, not raised, because going from one copy to three moves the whole stream across the
cluster, which is the operator's to schedule and not a side effect of whichever process started
first. That is the more cautious of the two and the easier to relax later: raising in place is one
`update_stream` away, and a deployment that came to rely on it would be the hard thing to take back.

**The count is read back after the create.** The check that refuses an old bucket also runs on one
this call just made, so a create that forgot to pass the count, or a server that quietly gave
fewer, is refused on the spot and not discovered on the day a node dies.

**`--nats-url` is a list.** One server or several, comma-separated, tried until one answers. A
client given all of them reconnects to whichever is up when the one it was talking to dies, so a
node lost later needs nothing more here, which is why a daemon should be told every server. An
empty entry is refused.

**Refused before any server is asked:** a count outside 1 to 5, and `--memory` with `--replicas`
(memory has no copies). **Passed through, with the server's own words:** one server asked for three
says `replicas > 1 not supported in non-clustered mode`, after naming the bucket and the number.

## What it was measured to do

[`crates/holon-park/tests/cluster.rs`](../../crates/holon-park/tests/cluster.rs) starts a real
three-node JetStream cluster and kills nodes with SIGKILL while tickets are written:

- Asked for three, the server has **three copies of both, on file storage**, caught up.
- Thirty tickets in a mix of states (parked, answered and announced, interrupted, cancelled, taken),
  then **the node that leads the bucket is killed**: every ticket is in the state it was
  acknowledged in, found by session; more are written, answered, announced and taken with two
  nodes; the node comes back on its own directory and catches up. Then **the node that leads
  `PARK_WAKE` is killed**: the same again, and every announcement nobody had taken is still queued,
  **once each**, across both losses.
- With `comp-resume`'s loop delivering, **the node that leads its consumer is killed**: all twelve
  answered calls reach the sink, six before the kill and six after, and each is closed.
- With one copy, the node holding it killed: the bucket is unreachable, as above, and the ticket
  is there again when the node is.
- A call parked through one server, answered through the next, and listed and taken through the
  third, each told about that server alone and each step straight after the last, three hundred
  times: every one found. This is the engine reading what it has just written through a different
  connection, which `wake` does by correlation the moment a webhook arrives. A bucket's reads are
  direct gets that any current copy may answer, so on three copies a read could in principle be
  older than the write before it; none was, here or in a throwaway probe of twelve thousand reads
  (each right after a write, through each node in turn). That is the steady state: a failover
  window was not probed for it.

How long a caller is refused is not what the test says, so it was measured on its own: killing the
leader of a three-copy bucket and polling with a 300 ms client timeout, **the bucket took no reads
and no writes for 2.8 to 6.5 s, median 4.0 s** (six trials, one machine, `nats-server` 2.15.1; reads
and writes came back together). A caller sees more than that, because a request to a stream with no
leader is not answered and the client's JetStream timeout is five seconds a try: in the test, over
21 runs and 42 kills, the first acknowledged write after the bucket's leader died came after 4.3 to
10.3 s (median 5.6), and the first acknowledged announcement after `PARK_WAKE`'s leader died after
4.1 to 10.3 s (median 10.3: two timeouts). Both are the cluster's and the client's, not this
crate's. A caller that retries through them is safe because every call is idempotent, which is what
the crash matrix proves.

**A third leader, found by the test failing once in about thirty runs.** Creating or changing a
stream or a consumer goes through JetStream's *meta* leader, which may be the node just killed. The
failure was `wake.consumer(…).unwrap()` in the test, which created its verifying consumer once,
straight after the second kill, and panicked when that node had also led the meta group. Reproduced
by hand: with the meta leader killed, creating a consumer timed out and went through **4.5 to 6.5 s**
later (three trials, a 1 s client timeout, retried under one name so a retry cannot collide with an
attempt that had landed); with any other node killed it went through at once or after one retry. The
test retries it now, like every other call. `comp-resume` creates its consumer once, at startup, so
a resumer *started* in that window can fail to start and is left to whatever supervises it; one
already running holds its consumer.

**The flag reaches the server**, which the library test cannot see: a daemon that parsed
`--replicas 3` and dropped it would start without a word. [`reconciler/tests/park_replicas.rs`](../../reconciler/tests/park_replicas.rs)
asks one server that cannot keep three copies and requires both daemons to be refused with its
reason, and the same daemon without the flag to start. It also covers a dead server first in the
list, and the usage errors.

By hand, with the real binaries on a cluster: a daemon started with no flag, then with it (refused,
as above), the remedy run as printed, and then accepted; and with the first server in the list
killed, still started.

**Mutation checks** say the tests can fail. A check that never refuses fails the refusal test; one
that matches exactly and not "at least" fails it too (a resumer on the default would be refused); a
create that ignores the count for the bucket, or for the stream, fails the cluster test; an empty
address accepted fails the single-server test; either daemon dropping the flag fails its own.

## What this does not do

- **No majority lost.** Three copies tolerate one node, not two. With two down the streams have no
  quorum and stop until one returns; JetStream's own account of that is the one to go by, and
  nothing here tested it.
- **No partition, no disk lost for good.** A node killed and restarted on its directory is what was
  measured. A node lost with its disk and replaced, which should resynchronise from the others, was
  not.
- **No power loss.** ADR-0106's second row is unchanged and was not re-measured: copies on separate
  hosts are separate flush windows, and a cut that takes every host at once is the case this does
  not help.
- **It does not raise an existing deployment.** Above, on purpose. The command is printed.
- **No say over which nodes.** Placement is JetStream's; tags and a named cluster are not exposed.
- **`infra/compose.yaml` still runs one node.** The flag is for a deployment that has a cluster;
  asked for on the compose NATS it is refused, with the server's reason.
- **CI does not run these.** They need `nats-server`, one of them a cluster of three, and live
  behind the `native` feature like `tests/live.rs` and `tests/restart.rs`: CI compiles and lints
  them, and they are run locally.

## Consequences

- **No contract change.** The count is a deployment setting, not part of what a caller can ask of
  park, so `holon:park` stays 0.4.0 and the WIT is untouched.
- **`connect` and `open` mean what they meant**: one copy. `connect_with_replicas`,
  `NatsParkStore::open_with_replicas` and `WakeStream::open_with_replicas` are new beside them.
- **Defaults change nothing.** A deployment that does not pass the flag is the one before this, with
  one difference: `--nats-url` now splits on commas, so an address with a comma in it is no
  longer one address.
- **`apps/park.toml`** says how to pass both, through `extra_args`.

## Alternatives

- **Raise the bucket in place when the flag asks for more.** Rejected for now, above. One
  `update_stream` call, read from the current configuration so nothing else changes, would do it.
- **Treat the number as exact.** Rejected: the daemon that did not pass the flag would be refused
  against a bucket that has three copies, which is the one mistake that takes a deployment down on
  restart.
- **Warn and carry on.** Rejected: the failure it would hide stays hidden until the day a node
  dies, which is the day it matters.
- **A parameter of the contract.** Rejected: nothing a caller does depends on it, and it would make
  the same ticket mean different things in different deployments.
