# ADR-0115 — The reconciler is a workspace of single-purpose packages

*A package's dependencies are built for every one of its targets, so a binary that needs nothing
paid for the whole tree anyway, and a daemon's heavy crate was the control plane's problem.*

**Status: accepted, and built one slice at a time.** `reconciler/` is a Cargo workspace; the
packages under it are the list of what has moved (`cargo metadata --manifest-path
reconciler/Cargo.toml`), and [the architecture map](../ARCHITECTURE.md) names where each system's
code lives now. The first slice is the workspace itself and `comp-daemon-kit`
([`reconciler/daemons/kit/`](../../reconciler/daemons/kit/)). Reverses the arrangement
`reconciler/Cargo.toml` used to describe in its own comments — "one native crate, many
single-purpose bins" — which was cheap while there were a few bins and stopped being so when
[ADR-0095](0095-what-is-allowed-to-be-native.md)'s daemons arrived. Changes no behaviour, no wire
format, no binary name.

## The problem

`comp-reconciler` was one package: a library and a long list of `[[bin]]`s, with every dependency any
of them wanted in one `[dependencies]`. Cargo builds a package's dependencies for *any* of its
targets, not for the one asked for. Measured on `a263cb95`:

- `cargo build --bin comp-mailrelay` — a binary whose own comment says "std-only, no dependencies" —
  compiled the package's whole tree first: 526 crates (normal and build), among them `bollard`,
  `image`, `headless_chrome`, `rawler`, `wac-graph` and the tonic family.
- `Dockerfile.daemon` builds one daemon per image, and each image compiled every daemon's
  dependencies.
- CI's two prewarm steps build `comp-plug` and `comp-mailrelay` only to warm the cache, which is to
  say they build the tree.
- A change to `plug.rs`, in the library, rebuilt every binary and every test, the daemons among them,
  though a daemon uses one module of the library.
- The package that holds the lattice credential declares `headless_chrome`, `arboard` and
  `notify-rust`, and no line of the reconciler touches them. Each declaration was a reasonable call
  when it was made — the Cargo.toml says why for `comp-ingress` and for the daemons' crates — and the
  sum of them is a package nobody can describe in a sentence, and an audit of it has to read past
  every daemon to find the control plane.

The thing that kept it one package was the tests. `CARGO_BIN_EXE_<name>` exists only for the
integration tests of the package that owns the binary, and a fleet test starts several of them, so
splitting binaries away from tests looked like breaking the suite. It is not, if the tests move with
the binaries they spawn and a test that wants a binary from elsewhere finds it the way it already
finds `comp-host`, which lives in another workspace and which CI builds once and hands to the jobs
that spawn it.

## The decision

**`reconciler/` is a workspace.** The root package stays `comp-reconciler` — the control plane: the
loop, the ingress, and the library `comp app plan` shares with them — and the other packages live in
subdirectories of it.

**A native daemon is its own package**, named for its binary and kept in `daemons/<name>/`, depending
on its one heavy crate and on nothing of the control plane's. What daemons genuinely share is a
package too, `comp-daemon-kit`, and it is as small as it started: the shared-secret check
([`daemon_auth`](../../reconciler/daemons/kit/src/lib.rs), moved unchanged). Anything added to it has
to be something *every* daemon needs; the day it needs a crate only one of them uses, that crate
belongs to that daemon.

**Nothing that runs the workspace changes.** `default-members` lists every package, so `cd
reconciler && cargo build --bin comp-x`, `cargo test --test x`, `cargo nextest run -E 'test(x)'` and
`cargo clippy --all-targets` mean what they meant — CI, the release workflow, the Docker build, the
e2e scripts and `xtask` all keep their commands. A package left out of that list still compiles, as a
dependency, and stops being tested, built by name and linted with nothing red; so
[`reconciler/tests/workspace.rs`](../../reconciler/tests/workspace.rs) fails when a member is not a
default member. There is one `target/`, here, so the many
`reconciler/target/release/comp-*` paths in scripts, gates and docs stay true, and binary names do
not change. (There is no committed `Cargo.lock` here, and the split does not add one.)

**A test lives with the binaries it spawns.** `CARGO_BIN_EXE_*` is the reason, and it is also the
cheapest way to be sure the binary the test runs is the one this commit built. A test that needs a
binary from another package finds it through `Fleet::bin_path`, which already works from the
executable's own directory (`built_bin` is the same with an assertion that says how to build it), and
the job that runs it builds that binary by name first. That is a
line in CI for each such binary, and it is why a binary that nothing tests can move at once while one
that a fleet test starts waits until the line exists. The harness the suites share
([`tests/harness/mod.rs`](../../reconciler/tests/harness/mod.rs)) is not copied into a package that a test
moves to but included by `#[path]`; it names `comp-fleet` and not the root package, so it compiles in
either place, and its `repo_root()` is the one the harness has everywhere.

**Dependencies point one way.** A package may depend on the root (for `fleet`, `plug`, the plan
types) until those get a package of their own; the root depends on a member only for a leaf such as
the kit. No package depends on a daemon, because a daemon is a process, not a library.

**A slice moves, and does not edit.** `git mv`, a `Cargo.toml`, the import paths, the pointers in the
docs. The check is mechanical and is in the pull request: `cargo nextest list` on the base and on the
branch, compared by name — every test the old tree had the new one has, in the same number, and the
only thing that may differ is the package a test is in.

**The order is leaves first**, because each slice teaches the next what it forgot:

1. the workspace and the kit (the first slice);
2. the daemons nothing tests, each with its one heavy crate;
3. the harness, as `comp-fleet` (`fleet` and `plug`): everything after it that starts a process needs
   it, and it is re-exported from where it was (`comp_reconciler::fleet`), so nothing that named it
   changes;
4. the daemons that have a test of their own, which move with it (`comp-backup`, `comp-media`,
   `comp-vcs`, and `comp-park` with `comp-resume`, whose tests start the pair and so share a
   package); `comp-agentd` after the small modules it reads (cost and off-peak pricing) have a
   package of their own, and its test that starts it with `comp-park` and `comp-resume` is a test
   across three binaries of two packages, so it stays in the root's suites and finds them by
   `Fleet::built_bin`;
5. the leaves nothing starts from outside a package (`comp-mailrelay`, `comp-agent`, `comp-relay`,
   the measuring tools), then the library the loop and the reconciler both read artifacts through
   (`comp-oci`);
6. the goal loop last, because it shares the most: its modules, its binaries and the tests that
   exercise them, as `comp-goal`.

## Where it stopped

The root package is the control plane and what its own suites start by name. `comp-reconciler` and
`comp-ingress` are the loop and the front door, over the three modules they are made of (`plan`,
`settings`, `spec`). `comp-stub` is the stand-in control plane a fleet can start instead of the real
one, `comp-bench` is what a fleet's benchmark report is read with, and `comp-checks` is the gate
runner every gate suite starts: a suite in this package starts each of them by name, and cargo builds
them for it as the package's own. `comp-planscale` reads the plan the reconciler is made of. Moving
any of those costs a line in CI and an edit in a test apiece — a fleet suite would have to build
`comp-stub` and `comp-checks` before it could run — and buys nothing while the root has a suite that
starts them.

The two command lines over libraries that live elsewhere, `comp-plug` (composition) and `comp-oci`
(artifacts), are `comp-fleet`'s: the library they wrap is there, and while they were the root's they
were the reason the control plane's tree carried `wac-graph` and `wasmparser`. The root does not
re-export the harness any more; the suites, the benchmarks, the goal loop and `gpui-console` name
`comp-fleet`, which is a dev-dependency of the control plane.

What the split took out of the control plane's tree, measured with `cargo tree -p comp-reconciler -e
normal,build` — and the same command is how to tell whether it stays true: the wasm toolchain
(`wac-graph`, `wasmparser`), the gRPC stack (`tonic`, `prost`), SurrealDB, the S3 signer, and every
crate behind a daemon (`bollard`, `arboard`, `headless_chrome`, `mdns-sd`, `notify-rust`, `image`,
`rawler`). Every crate only the suites use is a `[dev-dependencies]` entry. What it still carries, and
why: `async-nats`, through the lattice, which is what the reconciler talks to, and `comp-oci`, the
artifacts library the reconciler pulls with.

## What this does not do

It does not rename `reconciler/` or any `comp-*` (that is the `holon` rename, and a separate
decision). It does not split the integration tests from the fleet harness; they stay with `Fleet`.
It does not version, publish or release a daemon on its own — a package here is a unit of
compilation and of ownership, not a product. It does not change what a daemon does, says or
listens on.

## Alternatives

**A virtual manifest, with the control plane moved into a subdirectory.** The tidiest end state, and
the most expensive way to reach it: `repo_root()` is the manifest directory's parent, the tests build
their paths from it, and `gpui-console` depends on the package by path. It buys nothing the
`default-members` line does not.

**Cargo features, one per daemon, on the one package.** Features are additive and unify across a
workspace build, so building everything builds everything; `cargo build --bin comp-docker` would not
turn `docker` on by itself (`required-features` makes it an error); and every command above would grow
a flag. It moves the problem into the command line.

**Leave it, and cache harder.** The cache is why this was tolerable. It does not shorten a cold build
of one daemon image, and it does not make the credential-holding package's dependency list shorter.

## Consequences

A daemon's cold build compiles its own crate and the kit's tree (56 crates on `a263cb95`, against
the package's 526), not everyone's. A change to one daemon rebuilds that daemon. The package that holds the lattice credential lists the dependencies that are
the control plane's, and an audit of it can stop there. The cost is a `Cargo.toml` per package and
the discipline above: a new daemon is a new directory, not a new `[[bin]]`, and a dependency one
package adds is that package's.
