# Hardening audit

What a sweep of the repository found, what was fixed, and what was deliberately
left. Measured rather than reviewed: every number here came from counting, and the
ones that are still wrong are named.

The audit was not a checklist. It asked one question, borrowed from the four bugs
found the same week — *where else does a failure wear the return type of a
success?* — and that question found five more.

## The pattern

| | what it did | what it looked like |
| --- | --- | --- |
| a write over 4096 bytes | trapped mid-response | a closed socket, three layers away |
| a failed read | returned a truncated body | a complete body |
| exhausted retries | never committed | `Ok(…)` |
| a gate's `tail -25` | hid the compiler error | the candidate's fault |
| **a poisoned lock** | **killed the node permanently** | **a panic, forever, in unrelated code** |
| **an unbounded body** | **trapped the component** | **a closed socket** |
| **a failed ledger write** | **lost the fleet's desired state** | **nothing at all** |
| **a skipped test** | **verified nothing** | **a green suite** |

## Fixed

### 1. A poisoned lock made one panic permanent — 64 sites

`std`'s `Mutex` poisons itself when a thread panics while holding it, and every
later `lock().unwrap()` panics too. The KV store, the route table and the instance
map are all behind locks on the request path, so one transient panic anywhere meant
the next request failed, and every request after it. The process stays up,
answering nothing, which is the worst available failure mode.

Poisoning is a conservative signal, not a corruption detector: in safe Rust a panic
cannot leave a `HashMap` half-updated, so the data behind the lock is intact and
refusing to look at it buys nothing. `host/src/sync.rs` takes the guard anyway, and
its tests poison a real lock from another thread and read the value back.

Counted before: 64 (kv 26, agent 16, kvprofile 9, kvcache 7, main 6). After: 0
outside tests.

`parking_lot` would remove poisoning entirely and is the usual answer. It is a
dependency added to change three lines of behaviour, and this host deliberately
runs on very little.

### 2. No request body had a ceiling — 38 components capped

When this was measured the tree held 150 components, and two of them limited how much
they would read; the rest accumulated whatever arrived until the guest hit wasmtime's
64 MiB per-store cap and **trapped**
— which reaches the caller as a closed connection saying nothing about a size.

`MAX_BODY_BYTES = 16 MiB` is a backstop, not a content policy. It is deliberately
generous: an API that needs a real limit should state its own and answer 413, which
is exactly what `upload-drop` and `paste-bin` already do, so they and the other
binary-handling components were left alone.

38 capped, 13 have a different read shape and were left, 5 police it themselves.

### 3. The ledger discarded every failure it could have

`Agent::persist` carries this doc comment:

> *Persist what we were told to run, so a reboot is not a data-loss event for the
> fleet's desired state. Atomic rename: a half-written ledger read on the next boot
> would start a subset and look like a partial outage.*

Underneath it, a `let … else { return }` and an `if …is_ok()` with no `else`. A full
disk or a read-only directory made every persist a silent no-op, and the fleet found
out one reboot later — as the partial outage the comment describes.

It still does not propagate, because the caller is a heartbeat with nothing useful
to do and failing the heartbeat would turn "the ledger did not save" into "the node
is down". It says so instead, which is the difference between a degraded node and a
silently degraded one.

`load_ledger` had the mirror image: an unreadable or corrupt ledger became
`BTreeMap::new()`, which is indistinguishable from "nothing was running".

### 4. A skipped test reported as a pass

`just test` said "222 passed" whether or not Docker was up. Without a database the
suites proving the knowledge loop, the contract negotiation and every composed
deployment all skip — and the umbrella still said everything passed.

It cannot fail on a skip, because skipping is correct on a machine with no database.
It now counts them and prints what a green run did **not** verify.

## Not fixed, and why

**13 components read a body with a different shape.** They were left rather than
converted by hand, because a mechanical change to a shape you have not read is how
a hardening pass becomes an outage. They are findable: `grep -L MAX_BODY_BYTES` over
the components that define `read_body`.

**`.expect()` in host startup — 10 sites.** These are in `main`, before the host
serves anything: a missing binary, an unparseable address. Panicking there is
correct and the message is the interface.

**The 55 copies of `read_body` are still 55 copies.** Nine variants, but the top
three differ only in a variable name and a signature, and the bug they all shared is
fixed and linted. A shared crate would need a macro to reach each component's own
generated bindings, and would put a cross-crate dependency into 150 deliberately
self-contained components to remove boilerplate that is no longer wrong.

**Lock contention is unmeasured.** The poisoning fix says nothing about whether
these locks are a bottleneck. Nothing profiles it, so nothing here claims anything
about it.

## Clean

Stated because an audit that only reports problems is not an audit.

**Outbound deadlines.** 78 components set both `set_connect_timeout` and
`set_first_byte_timeout`; 13 make outbound calls. Every path out has a deadline.

**Secrets.** `comp:secrets/reader.reveal` logs the key NAME and never the value
(`host/src/main.rs`), `host/src/secrets.rs` has no path from a plaintext to a log
line, and secrets reach the runner as file paths rather than in argv (ADR-0010).

**Read loops.** All 55 are loops; none reads once and assumes it got everything.

## How to repeat it

```
# panics in code that serves requests, excluding tests
awk '/#\[cfg\(test\)\]/{exit} /unwrap\(\)|expect\(|panic!\(/{c++} END{print FILENAME": "c+0}' host/src/*.rs

# swallowed failures
grep -rn 'let _ = ' host/src reconciler/src --include='*.rs'

# what a green suite did not verify
just test 2>&1 | grep SKIPPED
```

The useful part was not the greps. It was picking one failure that had already cost
something and asking where else its shape lives.
