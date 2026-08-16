# ADR-0088 — what a gate says is what the next attempt reads

*A check's output is not a log. It is the input to the next generation's prompt. A
gate that reports the wrong thing does not merely mislead a human reading a
terminal — it spends money teaching a model to fix a problem that does not exist.*

**Status: the rule, plus two mechanical guards.** `components/clinic-domain/gate-lib.sh`
is the shared harness the rule is enforced in; `reconciler/tests/guestio.rs` is the
lint for the failure that hid behind a bad message for four runs. This ADR records
a rule that has now been broken in four distinct ways, each of which cost real
money, and each of which was invisible because the gate's own words were wrong.

## The rule

Everything a gate prints will be read by a language model that cannot see the
terminal, cannot re-run the command, and has no other source of truth about what
happened. So:

1. **A gate must not say "your code is wrong" when it means "I could not run."**
2. **A gate must show the evidence, not the aftermath.**
3. **A gate that passes on the base tree cannot judge anything**, and the run
   refuses to spend before checking this (ADR-0080, `compose::criticise`).

Rule 3 was already enforced. This ADR is about 1 and 2, which were not.

## The four times it went wrong

**Sixteen gate runs judged a broken harness.** `COMP_HOST` pointed at a binary
that did not exist, so every check failed with "no comp-host at …". Every one of
those failures was reported to the model as a failing candidate. One generation
read the message and wrote an essay about the build system instead of the file it
was asked for — a rational response to the only evidence it was given.

**Three generations judged against a quoting bug.** `[ "$(pcode … "{\"json\":…}")" = 409 ]`
— a nest of quotes inside a command substitution inside a test — made bash answer
`[: too many arguments`. That went into the report as the candidate failing.

**A part spent three rounds in each of two runs fixing an error it was never
shown.** The gate handed the repair prompt `tail -25` of the build log. On a rustc
diagnostic those 25 lines are the trailing macro notes, `= note: this error
originates in the macro …`, and `error: could not compile`. The type, the message
and the `--> file:line` have scrolled off the top. The gate now prints from the
FIRST `error` line, and printing it that way immediately revealed the actual bug,
which nobody could have guessed from outside the repo: serde is built here with
`default-features = false, features = ["derive", "alloc"]`, so `HashMap` has no
`Serialize` impl at all.

**Seven branches across two runs were killed by their own client and reported as a
fleet fault.** `--timeout` defaults to 300s; the reconciler hangs up; the host logs
`hyper::Error(IncompleteMessage)`; the ingress logs `connection closed before
message completed`; the run says `error sending request for url .../run`. Four
messages, none of which contains the word "timeout" or a number of seconds. The
flag's own documentation called 300s "generous" — measured, the gate is 2.3
seconds and the model calls have a median of 64s and a tail of 174s, so two of
those in one branch is the whole budget.

## Why this keeps happening

Every one of these is the same shape: **the layer that knows the cause is not the
layer that writes the message.** A trapped guest becomes a closed socket becomes a
JSON parse error. A cargo failure becomes 25 lines of whatever happened to be
last. A client timeout becomes a transport error at the server.

That is not fixable in general, so the rule is a discipline rather than a
mechanism: when a gate reports a failure, ask what a reader who can only see this
text would conclude, and whether that conclusion is true.

## The two mechanical guards

**`gate-lib.sh`.** The clinic's five gates were five copies of the same forty
lines, so the quoting bug and the `tail -25` bug each had to be fixed five times.
The shared parts now live in one file, and what a gate still writes for itself is
only the thing it judges. This does not make messages correct; it makes a
correction land everywhere at once.

**`reconciler/tests/guestio.rs`.** The failure that hid the longest was a
component trapping mid-write, because `wasi:io`'s `blocking-write-and-flush`
accepts at most 4096 bytes and TRAPS above that instead of returning an error. 30
of 91 write sites in this repository did that, including the clinic's own router.
It waited for a contract file to grow from 3645 bytes to 4573 and then took down a
real run twice over, with no message anywhere naming a size or a write. Every
guest write now goes through a `write_all` that asks `check-write` how much the
stream will take, and the lint fails on a new unbounded write — with an `ALLOWED`
list, because a provably-small write deserves somewhere to go that is not a
workaround.

## Consequences

* Adding a gate means adding assertions, not re-deriving a harness.
* A harness fault and a candidate fault must be distinguishable in the text alone.
  `gate-lib.sh` says "the gate cannot run what it built" for the first and
  "FAILED: <what was expected>" for the second.
* When a run reports something inexplicable, suspect the message before the model.
  Four times out of four so far, the message was wrong and the model was
  responding sensibly to bad evidence.
* `COMP_FLEET_KEEP_LOGS` exists because the fleet deleted the node logs that held
  the only accurate description of the failure, which is why the 4096-byte trap
  took four runs to find rather than one.
