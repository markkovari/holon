# ADR-0095 — what is allowed to be native

*The README says "only what must be native is native". Five things are, and until
now there was no written test for admitting a sixth.*

**Status: accepted.** Names the rule the tree already follows, and settles a
question that came up while planning host-backed capabilities.

## The gap

Every workload here is a wasm component with a WIT contract. That is the whole
design: components are the unit of isolation (a linker boundary, not a policy)
and the unit of composition.

And yet five things are native, each for a reason nobody wrote down in one place:

| native | why it cannot be a component |
|---|---|
| `host/` | runs the wasm. Something has to be outside the sandbox. |
| `reconciler/` | holds a NATS subscription and a timer. A component has no background. |
| `comp-checks` | spawns processes. A component cannot fork. |
| `comp-plug`, `comp-capgraph`, … | shell out to `wac`, read the filesystem. |
| the twelve host capabilities | a watch syscall, raw sockets, a browser, ffmpeg. |

Five instances and no rule. The sixth would have been argued from precedent,
which is how "only what must be native" becomes "whatever we did last time".

## The decision

**A thing may be native only when a `wasm32-wasip2` component cannot express it,
and the reason must be a missing CAPABILITY rather than a missing convenience.**

Three questions, in order. A "no" to any of them means it stays a component.

1. **Does it need something WASI does not give a guest?** A process, a raw
   socket, a syscall, a device, a background thread, a held subscription. Not
   "it would be faster" — faster is a convenience, and this repository has
   measured that a component start costs 0.43ms (ADR-0040).
2. **Is the thing it needs the SMALLEST it could be?** `comp-checks` spawns
   processes and does nothing else; the gate that decides what to run is a
   component. A native binary that also holds business logic has taken more
   than it needed, and every line inside it is outside the isolation boundary.
3. **Does it answer a contract a component could have answered?** If yes, the
   contract stays in WIT and the native thing satisfies it from the other side
   of a socket. `checks-runner` is a component that dials `comp-checks` over
   loopback under an egress allow-list; `wasi:keyvalue` is the same shape with
   the host on the far side.

Question 3 is the load-bearing one. It is what keeps the twelve host
capabilities as WIT contracts with daemons behind them, rather than as native
code that swallowed the interface too.

## What this does NOT license

**A shared Rust library for components.** It came up as the obvious fix for
~200 duplicated helper definitions — 72 copies of `read_body`, 54 of
`write_all`, 60 of `emit` — and it is not possible, for a reason worth writing
down so nobody re-proposes it:

`cargo-component` generates a `bindings.rs` per crate. `abtest-domain`'s
`IncomingRequest` and `arena-domain`'s `IncomingRequest` are the same WIT type
and **different Rust types**. A crate cannot name either in a signature. The
copies are not laziness; they are what the component model costs, and no
component in this tree depends on another crate by path.

So the control on those helpers is a LINT, not a library. `guestio.rs` already
carries three:

- no unbounded `blocking-write-and-flush` (it traps above 4096 bytes),
- a read loop tells end-of-body from a failed read,
- a body read into memory has a ceiling,

each with a named allow-list carrying the reason a site is exempt. That is the
right shape: the duplication is structural, the DIVERGENCE is the defect, and a
lint catches divergence where a library cannot exist.

A macro crate could deduplicate the text. It is deliberately not adopted: it
would put the one thing every component's HTTP path depends on behind an
expansion, in a repository whose components are otherwise readable end to end.
Revisit if the lint list grows past what a reader can hold.

## Consequences

- A new native thing needs the three questions answered in its own doc header,
  and the answer to 3 is usually "here is the WIT it satisfies".
- The twelve `UNIMPLEMENTED:` capabilities get daemons, one per capability, each
  with its own allow-list — `container-docker` and `ui-notifier` do not deserve
  the same blast radius, and one daemon for all twelve would give them one.
- The helper duplication stays, and stays linted. Anybody proposing to collapse
  it should read the paragraph above first.
