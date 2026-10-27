# ADR-0118 — The `comp` prefix is retired one layer at a time

*The repository, the CLI, the console and the libraries written from ADR-0099 on are called Holon, and the
platform underneath still answers to `comp`. A name with two spellings costs a decision every time
something is named, and renaming all of it at once would break every wire it appears on.*

**Status: accepted.** Layer 0 (the test guard against new `comp-` crates) and Layer 1 (first group of leaf daemons: `ffmpeg`, `docker`, `imageopt`, `llmlocal`, `mdns`) are in progress. Each layer is its own PR.

## What carries the name, counted

Counted with `git grep` over tracked files, leaving out lockfiles, images and the generated architecture
page. A count is a measurement of the day this was written and is not kept up to date.

| layer | what it looks like | files | what depends on it |
|---|---|---|---|
| packages and binaries | `comp-host`, `comp-agentd`: 30 packages, 49 names with their binaries | 730 name a `comp-` anything | CI, `Dockerfile.daemon`, `release.yml`, the launchd units, tests that start a binary by name (`bin_path("comp-checks")` and ten other names), the docs |
| Rust identifiers | `comp_fleet::fleet` | 161 | the packages above |
| environment variables | `COMP_*` | 87 | the units, scripts and shells that set them |
| the repository's own directory | `.comp/goals/` | 74 | the goal loop, the docs |
| a person's secrets directory | `~/.comp-secrets` | 20 | the person's machine: a Google token, a Jev key |
| the configuration file | `comp.toml` | 9 | whoever operates a node |
| the app spec's version | `comp/v1` | 57 | every authored manifest and the tests of it |
| NATS subjects | `comp.v1.<lattice>.cmd.<node>.<verb>` | 8 | a reconciler and the hosts it commands, whatever version each is |
| WIT packages | `comp:store`, `comp:secrets`, `comp:auth`, and eighteen more, most of them test probes | 160 | every component built against one, and the host that provides it |

The other spelling is already here, and was not chosen so much as typed: the CLI is `holon`, the pure
libraries in `crates/` are `holon-vcs`, `holon-park` and `holon-llm`, the park contract is the WIT package
`holon:park`, the release archive is `holon-worker-<target>.tar.gz`, and about two dozen files read
`HOLON_*` variables. The daemons added in the same weeks are `comp-park`, `comp-resume` and
`comp-agentd`. The line between the two has been drawn by whoever was writing.

## Two kinds of name

A **label** is what a person types or reads, and is rebuilt together: a package, a binary, a directory in
the repository, a page of documentation. Renaming one is a commit.

An **identity** is what two parties built at different times have to agree on: a WIT package, a NATS
subject, a manifest's `version:`, an environment variable a deployed unit sets, the path a person's data
is under. Renaming one is a migration, and half of one is an outage: a host that provides `comp:store`
and a component that imports `holon:store` do not meet, and the error names an import nobody provides, not a rename.

## The decision

**Nothing new is named `comp`.** A package, a binary, an environment variable or a WIT package added from
here on is `holon-…`, `HOLON_…` or `holon:…`. A test fails when a `comp-*` package appears that is not on
a list of the ones that exist, and the list only gets shorter.

**Labels are renamed in groups, and a rename is all a rename PR does.** As
[0115](0115-the-reconciler-is-a-workspace-of-single-purpose-packages.md) split the reconciler: one PR per
group of packages, the leaf daemons first and the core last (host, lattice, ingress, reconciler, and the
worker archive). Each changes a group's package and binary names and every place that spells them,
and nothing else; each is checked by what checked the splits: test parity by name, the docs and map guards,
and the by-name start sites, which are what finds a spelling that was missed. A script rewrites an explicit
list of names and not a pattern, because `comp-` also begins words nobody is renaming.

**An identity changes when its version does, and not before.** `comp/v1` stays until there is a `v2`,
which is then `holon/v2`. `comp:store@0.1.0` stays until that interface breaks, and its next major is
`holon:store@1.0.0`. The NATS prefix `comp.v1` stays the same way, and when it moves the hosts subscribe to
both for a release. Renaming any of them alone buys a spelling and costs every component rebuilt and
recomposed, and a window in which half a fleet cannot hear the other half.

**Where a reader can accept both, it does, for one release.** An environment variable: `HOLON_X` wins and
`COMP_X` is still read and said once. A directory in the repository: `.holon/` first, `.comp/` after.
**Nothing moves a person's secrets directory.** `~/.comp-secrets` stays where it is, is read where it
was, and anything new is written where the new name says, by a command someone ran.

**The order:**

| | step | what is true after |
|---|---|---|
| 0 | the rule, and the list a test holds it to | no new `comp-` |
| 1 | leaf daemons, by group of about five | the daemons are `holon-…`, their start sites and units follow |
| 2 | `fleet`, `goal`, `measure`, `fuel`, `oci` | the library packages |
| 3 | the core, and the worker archive, which carries both binary names for one release | the control plane |
| 4 | environment variables, both read | `HOLON_*` documented, `COMP_*` still works |
| 5 | the repository's directory and the paths | `.holon/` |
| 6 | identities, each with its next version | never on their own |

## What this does not do

It does not rename the repository (it is `markkovari/holon`), the `holon` CLI, or `components/*`, which
have no prefix. It does not decide a product name above this level, or rename anything in a wire format
that a deployed fleet is speaking today.

## Alternatives

**All at once.** One PR over the 730 files, and the ones among them that name a `comp:` interface are
components that all have to be rebuilt and recomposed against a host that provides the new names, while
the fleet that is running still asks for the old ones. The part of it that is a rename is a day; the rest
is a migration, and it has no way back.

**Leave it.** Two names for one thing, a naming decision with every new package and documentation that
spells both. The cost grows with the number of packages, which is 30.

**Labels now, identities never.** This is the plan, with "never" being "when the interface changes
anyway": the identities are the part where a rename is not a rename.

## What it costs

- `git blame` stops at each rename, as it already does at the splits of 0115.
- A branch in flight conflicts with the group that touches it. The groups are small so that it is
  one branch for one PR.
- For one release, a consumer of the worker archive finds both names in it.
