# ADR-0136 — Every node runs components and agents; components are built where a toolchain is and shared as artifacts

*The Pi (malna) ran one app, on a comp-host too old for it, and no agents; picur ran everything
else. Both should run components and agents, build what they can, and share components and what
the agents know. Most of the component half already exists — CI composes and publishes every app
by digest and `comp-agent` pulls it — and it broke in one place: nothing keeps the host itself
current. The agent half does not exist yet.*

**Status: proposed.** The deploy in *What was done* is live; the rest is the plan.

## What a node is

A machine on the tailnet with `comp-host` (systemd on Linux, launchd on macOS) and, optionally, an
`agent-runtime`. Today: **picur** (macOS arm64: the runtime, the Matrix bridge, vcs, the Apple
model, on-device speech, Calendar) and **malna** (a Raspberry Pi 5, Linux aarch64, 8 GB: Synapse,
NATS, events, llm-queue).

## The decision

1. **Components are shared as artifacts, never rebuilt per node.** A `.wasm` runs on every node,
   so a node needs no toolchain to run one: CI composes and publishes each app to
   `ghcr.io/markkovari/holon-apps` and the `deploy` branch's `apps.lock` names the digest
   (`publish-apps.yml`); each node's `comp-agent` keeps the apps it has a unit for at that digest.
2. **A component is built where its toolchain runs.** Rust builds on every node. MoonBit's
   toolchain ships for macOS and x86_64 Linux only, not aarch64 Linux, so a MoonBit component made
   on the fly is built on picur (or CI) and shared like any other; a Pi runs it, it does not build
   it. A node-built component reaches the others through the code store (`vcs`, ADR-0134) and the
   registry, the same path a CI-built one takes.
3. **`comp-agent` keeps the host current too.** malna's `comp-events` crash-looped because
   `comp-agent` pulled a p3 events build while `/usr/local/bin/comp-host` was a pre-p3 binary from
   weeks before: the lock pins apps, not the host that must run them. The lock gains one line per
   platform (`host linux-aarch64 sha256:…`), published with the apps, and `comp-agent` installs a
   new host (copy, rename, restart its units) before any app that needs it.
4. **Agents run on the node that has what they need.** `agent-runtime` builds for Linux aarch64
   (done, below). An agent whose capabilities are Mac-only (the Apple model, on-device speech,
   Calendar) stays on picur; one that needs only HTTP, the free LLM queue and tools runs on
   whichever node has room. Each runtime keeps its own state directory; an agent lives on exactly
   one node at a time, and moving it is copying its directory while it is stopped.
5. **Knowledge is shared over NATS, the bus every node already has.** An agent's memory items and
   finished runs are published as events (`holon.knowledge.<project>`); every runtime keeps the
   ones for projects its agents belong to. A project is the unit of sharing, as it is the unit of
   membership in Matrix (ADR-0105). *Open:* whether a memory written on two nodes at once is merged
   by last write or kept as two; measured before decided.

## What was done (2026-11-07)

- An aarch64 Linux `comp-host`, `holon` and `agent-runtime`, built in an arm64 Debian 12 container
  on picur (`rust:1-bookworm`, matching malna's glibc).
- malna's `comp-host` replaced (the old one kept as `comp-host.prev`): `comp-events` is up again.
- `llm-queue` installed on malna beside its NATS (`holon node render apps/llm-queue.toml`),
  answering `holon.llm.ask`. It has no provider keys yet: each is a `<provider>-api-key = …` line in
  `/etc/comp/llm-queue.env`, added by the operator.

## Not built yet

The host line in `apps.lock` and `comp-agent` acting on it; `agent-runtime` as a unit on malna (its
binary is built); the knowledge events. Each is its own change, in that order: the first removes
the failure that already happened.
