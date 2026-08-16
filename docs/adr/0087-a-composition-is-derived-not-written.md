# ADR-0087 — a composition is derived, not written

*A component states what it imports. Every capability states what it exports. That
is a complete wiring diagram, so nobody should be typing `wac plug … --plug … --plug
…` by hand — and nothing the loop builds should need a human to edit a build file
before it can run.*

**Status: built, used by every clinic gate, and proven against the hardest case in
the repository.** `reconciler/src/plug.rs` wraps `wac_graph` as a library:
`Catalog::scan` reads every built artifact's surface, `wiring` says who plugs into
whom, `compose` produces the artifact, `compose_to` keys it by content.
`comp-plug` is a thin shim for callers that are not Rust — a gate is a shell
script and cannot call a Rust function — and `just plug <name>` is the entry
point. Four tests, one of which composes `vet-domain` the flat way and asserts
that capabilities ARE left dangling, because that is the reason the recursion
exists.

## The phenomenon

Every showcase in this repository was assembled by a bespoke line in the
`Justfile` — 59 of them at the time of writing — plus one declarative
`compose.wac` whose `--dep` list is still typed by hand:

```
compose-vet: compose
    wac plug {{vetdomain_wasm}} --plug {{guard_composed}} --plug {{recordstore_wasm}} \
      --plug {{validate_wasm}} --plug {{searchindex_wasm}} --plug {{staticassets_wasm}} \
      -o {{vet_composed}}
```

So the recipe for assembling an app lived beside the app in a build file rather
than IN the app. Two things follow, and the second is the one that matters:

1. The list goes stale. `just compose-vet` names five plugs for a component that
   imports twenty-two capabilities, and the sixteen it omits — `ai:inference`,
   `blob:store`, `money:amount`, `otp:totp`, `csv:codec`, and eleven more — are
   simply left dangling in an artifact that `wasm-tools validate` is perfectly
   happy with.
2. **A component the agentic loop builds cannot be composed, run or deployed until
   a person edits the `Justfile`.** A substrate whose entire thesis is composition
   should not need a human to spell the composition out, and an agent that reaches
   for a capability nobody handed it should not be blocked on a build file it is
   not allowed to write.

## The decision

Derive it. Read the component's imports out of the built artifact, find what
exports those interfaces, and plug them.

Read out of the **binary**, not out of `components/*/wit/`, for three reasons that
were each found the hard way:

* `auth-guard` has no `wit/` directory at all — it targets a world in the shared
  root `wit/`. A scan of component WIT directories cannot see it.
* A source tree can declare a package it does not actually export.
* **The compiler drops an import nothing calls.** This one turned out to be a
  feature: it is what lets a gate ask "did this part actually USE the capability,
  or did it reimplement it?" — a question the composed artifact cannot answer,
  since plugging a provider *satisfies* an import and removes it just as
  thoroughly as never calling it did.

## Two things it gets right that a shell version got wrong

Both of these were written wrong first, in bash, and both are now pinned by tests.

**A flat plug chain is not a composition.** `wac plug root --plug a --plug b`
satisfies the ROOT's imports and hoists each plug's own imports into the result.
The "composed" vet clinic still imported `audit:log`, `ratelimit:guard` and
`llm:inference` — and validated cleanly. That is exactly why the `Justfile`
pre-composes `auth-guard` into `guard_composed` as a separate step by hand;
`compose` recurses instead, so every plug goes in whole, and
`a_flat_chain_would_leave_them_dangling` fails if that ever stops being true.

**Resolution is per-interface, not per-package.** `cache-backing` exports
`cache:store/sink` and `cache:store/source` but not `cache:store/cache`. A
package-level match reports "satisfied" for an import that then dangles through
the whole composition.

## Why a library rather than a driver around the CLI

`wac` is a Rust crate before it is a command. Shelling out means the answer
arrives as text to be parsed, every caller needs the binary on `PATH`, and the
interesting parts — which plug satisfies which import, what is unsatisfied,
whether the graph is buildable at all — have to be recovered from stderr. As a
library call, the loop can compose a candidate in-process and get the wiring, the
gaps and the refusal as values.

`components/wit-reflect` wraps the same crate for the **component** side, where a
sandboxed app inspects and composes without a host, and ADR-0005 and ADR-0006
already make it the authority there. This is the native side, for the loop and its
gates. That is the same split this repository already runs between
`checks-runner` (component) and `comp-checks` (native): native only where a
component cannot reach — a directory, a process, a shell script.

## What this does not do

Decide whether a plug's TYPES fit. It matches interface names and lets
`wac_graph::plug` refuse what does not fit, so the composition itself is the
check. `wit-reflect` exposes `wac`'s own `SubtypeChecker` through `satisfies` for
a caller that needs the answer BEFORE composing — a UI drawing an edge, say — and
that is the right place for it.

It also does not convert the 59 existing chains. They work, they are explicit, and
rewriting them would be churn for its own sake. `just plug` is what new work uses,
and an old chain gets converted when it next breaks.

## Consequences

* A component the loop builds is runnable and deployable with no human edit. This
  is the point.
* Composed artifacts are content-addressed under `components/target/composed/`, so
  a gate that runs twenty times composes once and the artifact outlives the run
  that made it — which is what makes it something to push to a catalogue rather
  than a temporary file.
* The derived composition is strictly more complete than the hand-written one, and
  `tests/contracts.rs` now checks the whole catalogue from both sides: every
  import has a provider (0 orphans across 150 components), and the provider side
  is reported so that a frozen interface is visible as one — `records:store/store`
  has 37 consumers.
* A gate can ask what a candidate actually called. `e2e-access.sh` and
  `e2e-reports.sh` fail a part that reimplements a capability the world already
  carries, which is a thing no behavioural test can detect.
