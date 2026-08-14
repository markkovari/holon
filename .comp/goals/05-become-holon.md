# Become Holon — 🔴 human-led (the first dogfood)

**Traces to:** this repository's own name. Internally everything is still `comp`
— the CLI binary, the lattice and app names, the doc references — because the
split preserved history byte for byte and a rename that broke the build on day
one would be a bad first commit.

## What is wanted

Rename the project from `comp` to `holon`, everywhere it is safe:

- the CLI binary `comp` → `holon` (Cargo `[[bin]]`, the `Justfile`, the docs that
  invoke it)
- the default lattice/app naming where it is cosmetic, NOT where a rename would
  change a store bucket a running deployment depends on (ADR-0023: a bucket is
  named after its app — renaming a live app orphans its data)
- prose and headings across `docs/` and the top-level app guides

Leave the WIT package names (`graph:agent`, `comp:store`, `comp:secrets`) alone:
those are content-addressed contract identifiers, and renaming one is a breaking
change to every component that imports it, for no functional gain.

## Why it is human-led

A rename looks mechanical and is not: the dangerous cases — a store name, a WIT
package, a signed digest — look identical to the safe ones in a grep, and telling
them apart needs to know which names are *boundaries* (the one rule in
`docs/CURRENT.md`) and which are just labels. That judgement is exactly what the
agent's whole-file, test-gated loop cannot make, and exactly what the person who
understands the boundary can.

The satisfying version of this goal: once the engine can run a multi-file,
`cargo build`-gated change safely, Holon renames itself. Until then, a person
does it — carefully, and in one reviewable pull request.
