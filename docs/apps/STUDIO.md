# studio — components describe themselves, and compose themselves

This repo assembles 109 components by hand: **55 `wac plug` lines** in a Justfile,
and a `tools/gen-catalog.py` that scrapes WIT with **five regexes** to build a
catalog nothing consumes programmatically. Both work. Both are blind the same way —
nothing in the loop reads the *binary* contract, so nothing can answer the only
question that matters when you wire two components together: **would this plug
actually fit this socket?**

`studio` is that question with a canvas around it. Drop a `.wasm` in and it is
**inspected**, not declared. Drag an export onto an import and the connection is
allowed only where `wac`'s own type checker says it fits. Then read the same graph
back as a `wac plug` script, a declarative `.wac` file, or a wasmCloud
`WorkloadDeployment` — and press **Compose** to get a real composed component.

![The studio: a left palette listing 109 reflected components with sizes, a dark canvas holding four component nodes (mesh-domain, record-store, resilience, proxy-route) whose ports are their actual WIT interfaces, and a right panel. Dragging export handles onto matching import handles draws emerald edges and flips the plan from “Unsatisfied (3)” to “Every composable import is wired”. Tabs switch the same graph between a generated wac plug script, a declarative .wac file, and a wasmCloud v2 WorkloadDeployment. Compose returns a real composed .wasm. A live recording of the running React + xyflow app.](../media/studio.gif)

## The component (why `wit:reflect`)

```wit
inspect:   func(bytes: list<u8>) -> result<surface, reflect-error>
satisfies: func(socket: list<u8>, plug: list<u8>) -> result<list<string>, reflect-error>
plan:      func(nodes: list<node>, edges: list<edge>) -> composition-plan
compose:   func(parts: list<part>, edges: list<edge>, root: string) -> result<list<u8>, compose-error>
emit-plug-script / emit-wac / emit-workload
```

**`inspect`** reads a component's own import/export sections with `wasmparser`.
Interface names in a component binary already *are* the strings everything else
keys on — `records:store/store@0.1.0` — so this is a parse, not an inference. Three
things fall out that the regex catalog cannot know:

| | `gen-catalog.py` | `wit:reflect` |
|---|---|---|
| import versions | stripped (`.split("@")[0]`) | intact — `wasi:keyvalue/store@0.2.0-draft` |
| host vs composable | `wasi:keyvalue`, `wasi:config`, `wasi:blobstore` filed as capability deps | separated: a **host** provides these, no component can |
| worlds | never captured | n/a — the built binary has no world name to capture (see below) |
| nesting | — | counts nested instances, which is what the 30-instance ceiling counts |

**`satisfies`** is the interesting one. It runs `wac-graph`'s `SubtypeChecker` — the
exact test `wac plug` applies — so the canvas refuses an edge for the same reason
the build would. Matching interface *names* is not enough: two
`foo:bar/baz@0.1.0` interfaces with different function signatures do not fit, and
this is where you find that out instead of at deploy time.

**`compose`** calls `wac_graph::plug`, which is the function the `wac` CLI is built
on. The bytes it returns are the bytes `wac plug` writes — the e2e proves it by
running the studio's own emitted script and comparing.

## What a built component won't tell you

Its own name. The embedded type of every component in this repo reads:

```
package root:component;
world root { import records:store/store@0.1.0; ... }
```

The source world (`mesh-domain`) and package (`mesh:app@0.1.0`) are **gone** —
`wac plug` doesn't need them, so `wit-component` doesn't keep them.

What's left is the `component-name` custom section, and on `wasm32-wasip2` **nothing
writes it by default**: `wasm-component-ld` doesn't, where cargo-component's old
adapter path did. This repo's `just build` stamps it back on with `wasm-tools
metadata add` (~35 bytes, idempotent), so components built here do report a name —
but treat it as a hint. A p2 component from anywhere else arrives anonymous.

The identity you can actually rely on is **what the thing exports**: a capability is
recognisable because it exports `records:store/store`, while an app that exports
only `wasi:http/incoming-handler` is unidentifiable either way. That's why the studio
takes an id on upload, and why `.wac` output names such a component
`<id>:component` — the same convention `components/login-app/compose.wac` uses.

## One graph, three forms, and they are not equivalent

This is the part worth having a tool for. The differences are real and quiet:

| | `wac plug` script | `.wac` file | v2 `WorkloadDeployment` |
|---|---|---|---|
| when | build time | build time | run time |
| shared instances | **no** — each socket gets its own copy of a plug, with its own state | **yes** — one `let`, shared by every consumer wired to it | n/a (separate components) |
| a diamond | silently becomes two instances | stays a diamond | stays a diamond |
| cycles | impossible | impossible | **legal** — the runtime links at invoke time |
| composable edges | erased into the artifact | erased into the artifact | **absent from the manifest** — same-workload components are linked in-process |
| host imports | survive; a host must supply them | survive | become `hostInterfaces` |

Two traps the emitters handle explicitly:

- **`wac plug` satisfies more than you drew.** It matches *every* common interface
  between a plug's exports and the socket's imports — it cannot be told to wire
  just one. Draw one edge to `auth-guard` and you get `authorizer`, `accounts`,
  `session`, `rbac` and `types`. The plan reports these as `also_satisfies`, and
  the generated script writes them into a comment. A UI that hid this would lie.
- **One `hostInterfaces` entry per interface.** An entry binds to a component only
  if that component's world covers *every* interface listed, so a merged
  `[store, atomics]` entry silently skips components importing only `store`. The
  emitter never merges.

## What it warns you about

- **The nested-instance ceiling.** wasmtime refuses to instantiate past ~30 nested
  component instances. `vet-domain` fused whole is 104 modules and **does not
  deploy** — which is why `vet-domain-lattice` keeps its stateful capabilities as
  links. The plan sums the instances and says so before you build.
- **Cycles** — refused for both static forms, with the message that a workload can
  express them.
- **Gaps** — a composable import with no edge stays an import of the finished
  artifact. Amber handle, listed by node and interface.
- **Edges that cannot exist** — an interface the plug doesn't export, a host
  capability dressed as an edge, a self-plug, a node that isn't on the canvas.

## Run it

```bash
just host-studio    # composes, builds the SPA, serves :3054, seeds all 109 components
# click components in to place them, drag an export handle onto a matching import,
# then read the wac plug / .wac / workload tabs and hit Compose.

just e2e-studio           # the whole ladder, against the real wac + wasm-tools
cd components && cargo test -p wit-reflect   # 13 tests, no host
```

A component cannot read the filesystem — the host preopens no directories — so
reflection has to be **fed over HTTP**. `just seed-studio` POSTs every artifact in
`components/target/wasm32-wasip2/release`; re-running replaces rather than
duplicates. You can also drop any `.wasm` onto the canvas from outside the repo.

## The e2e checks the claims, not the studio's opinion of itself

`examples/studio/tests/studio.rs`: reflect the repo's own artifacts; refuse
`zip → mesh-domain` because `satisfies` returns nothing; plan the mesh graph and
get one step, one root, zero gaps and 18 host needs; emit all three forms and
assert the shapes; then

- `POST /api/compose` → `wasm-tools validate` accepts it;
- run the **emitted `wac plug` script** with bash → same artifact size;
- run the **emitted `.wac`** through the real `wac compose` → composes and validates;
- serve the composed component on the host and call it → it answers as `mesh`, the
  guarded call really dials out, and the breaker's state persists through the
  `records:store` the studio plugged in.

That last one is the point: a graph wired in a browser produced a working app.

## What this does *not* do

- **No v1 OAM link traits.** Only the v2 `WorkloadDeployment` form is emitted. The
  1.x dialect with `link` traits per edge is a different model (and this repo
  documents *two* mutually incompatible spellings of it); adding it is an emitter,
  not a redesign.
- **One canvas node per component.** Two separate instances of the same capability
  aren't expressible. The interesting case — one capability shared by two consumers
  — needs no duplicate node: it's one node with two outgoing edges, which is
  exactly where `.wac` and `wac plug` diverge.
- **The instance count is an estimate**, `sum(1 + nested)` per node, and the limit
  (30) is a constant here rather than something read from the host.
- **No registry push and no deploy.** The workload manifest names images it does
  not build; `wkg oci push` and `kubectl apply` stay in the Justfile where they can
  see a cluster.
- **`wit-reflect` is 1 MB** — the largest non-asset component in the repo, because
  it carries `wasmparser` and the whole `wac` composition engine. That is the price
  of composing for real instead of printing instructions, and it only pays it in a
  dev tool.
- **Attestation of what came back**: the studio reflects its own output to report
  the leftover host imports, but it does not run the composed artifact for you.

## Rungs left

- **The v1 OAM emitter**, so the same canvas targets a 1.x lattice.
- **`wkg oci push` from the studio**, turning a canvas into a deployed workload in
  one step (the manifest already names the images).
- **Replace `gen-catalog.py`** — `wit:reflect` already knows everything
  `catalog.json` claims, and more, from the artifacts rather than the source.
- **Show interface detail on hover.** `inspect` stops at interface names; the full
  WIT (functions, records, resources) is in the binary's type section and
  `wit-component::decode` would read it, at the cost of another megabyte.
- **Diff two graphs** — "what changed between this composition and the one that
  shipped" is a question the surfaces can already answer.
