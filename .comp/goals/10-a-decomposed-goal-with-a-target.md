# A decomposed goal with a target — 🟢 agent-ready

**Traces to:** `docs/CURRENT.md` — *"A decomposed goal has still never been
DELIVERED. Two paid runs of the clinic's phase two, 290k tokens, no pull request.
… Both are fixed and unspent — the next run is the test of it."*

The fixes are unspent. The **target** was not, and that half of the sentence was
wrong: every decomposed goal in this repository has its parts implemented —
`triage`, `triage-assist`, `moderation-queue`, `support-desk`, `treasury-ledger`,
`invoice-copilot`, `doc-search-agent`, and the archived clinic. Each one is now
*refused* rather than run, because goal 07's base pre-check finds every gate passing
against the untouched tree. There was nothing left to spend a run on.

This is a target. `components/dispatch-domain` — a field-service dispatch API, three
parts, one contract, nothing implemented.

## What is different from `triage`

Triage's three parts each import a capability the others do not, so a part that
reimplemented one failed **alone**, in its own gate. That is a good gate and it
tests one part at a time.

Here `geo:resolve` is imported by **two** parts:

- `schedule` picks the nearest engineer with `distance-meters`
- `manifest` filters by radius with `bounding-box` and `contains`

So a part that hand-rolls haversine can be internally consistent, pass its own gate,
and disagree with its sibling. The composition gate compares the two numbers
directly — the distance `schedule` wrote has to be the one `manifest` prints, and
the request `schedule` placed has to fall inside a radius `manifest` measured.

That is a failure **three** independent parts can produce and **no single part's
gate can see**, which is the thing this goal is for. Two halves cannot produce it;
that is why the clinic never could.

`geo:resolve/coords` is also, at the time of writing, imported by nothing in this
tree — one of the sixteen `reconciler/tests/contracts.rs` reports as exported but
unconsumed. A landed run here is the first real consumer of a capability that has
only ever been checked against itself.

## Why 🟢 and not 🟡

The gates exist and fail. All four, against the untouched base, for the right
reason — the component composes, boots, serves, and answers `501 not_implemented`:

```
$ cargo test --release --test gate_dispatch
test result: FAILED. 0 passed; 4 failed
  requests_masks_validates_and_deduplicates      501 != 201
  schedule_assigns_the_nearest_and_refuses_…     501 != 200
  manifest_counts_quotes_and_filters_by_radius   501 != 200
  the_whole_dispatch_api_works                   501 != 201
```

That is the 🟡 already paid: the failing test written first, before anything is
spent on a model.

## What had to be fixed to write it

The gates are **Rust**, and they are the first goal checks that are. Every other
goal spec in this directory points at the shell copies of gates that were ported to
Rust in #180–#189, and the reason was not inertia: `gatelib` resolved `comp-host` by
path, while a goal run materialises a candidate in a sandbox holding the base tree
and nothing else and passes the binary as `$COMP_HOST`. So under a goal, every
ported gate found no host, **skipped** — and a skip returns `None`, which every gate
turns into `return`, which is a pass.

Thirty-one gates that pass because nothing was there to judge with is the
empty-corpus candidate wearing a different hat. `gatelib::host_bin` now reads
`$COMP_HOST`, and a missing binary or an empty catalogue **panics** under a goal run
while still skipping in a cold checkout — the asymmetry being the point, since only
one of the two has a score being written down.

## To run it

```bash
cp .comp/goals/dispatch.toml .comp/goal.toml
CHECKOUT=$PWD REPO=<owner>/<name> bash goal-demo.sh real
```

Through the Claude CLI shim, note the three timeouts in a row and that the shim's is
the lowest — `CLAUDE_TIMEOUT_MS=1500000 just claude-shim &` and `TIMEOUT=3000`, or
six branches come back `errored` at exactly 540006ms with the branch budget never
reached.
