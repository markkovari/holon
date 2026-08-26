# Getting a component you did not build

```bash
just pull portfolio-value-c                    # by name
just pull portfolio-value-c@sha256:…           # by digest, which cannot drift
just pull price-history-py /tmp/ph.wasm        # somewhere else
```

Anonymous by default. `OCI_USER` / `OCI_PASSWORD` for a private registry,
`OCI_REGISTRY` to point somewhere other than `ghcr.io/<owner>/holon`.

## Why this exists

Until now there were two ways to obtain a component's bytes, and neither was "get
this one".

**Build it.** That used to mean one Rust toolchain. Since `docs/POLYGLOT.md` it can
mean a 200 MB wasi-sdk, a Go toolchain and a wasip1 adapter, a vendored
SpiderMonkey, a CPython bundler — or, for the C# reproduction, a gigabyte of .NET.
Nobody should install a compiler to consume a 60 KB artifact.

**`just fetch-components`.** Reads the GitHub Actions artifact from a green run.
That is genuinely useful and it has three limits written into it: the artifact
expires after **thirty days**, it needs a successful run for **that exact commit**,
and it arrives as **all 205 components or none**.

A registry has none of those properties. `comp-oci pull` gets one component, by
digest, from bytes that do not expire.

## What is checked, and what is not

**Checked.** The bytes are hashed and compared against the digest the manifest
named, before anything is written. When the reference *is* a digest, the manifest is
checked against it too. ADR-0024 makes the digest the trust boundary and the store a
cache; a registry handing back the wrong thing is caught here rather than by
wasmtime, later, on someone else's node. There is a test that makes the registry
lie about one layer and asserts the pull refuses it.

**Not checked.** *Who* built it and *what from*. ADR-0073 signs a digest with an
organisation's P-256 key inside the platform's catalogue, and nothing in this path
consults that. A pull today proves the bytes are the bytes that were asked for —
integrity, not provenance. That is the next thing, not a thing that is done.

## Digests, not tags

Every push writes a tag: the first twelve hex of the component's own sha256. So a
tag exists for a human to type and **can never change meaning under someone**, which
is ADR-0006's rule. What a push *prints*, and what `--lock` records, is the digest.

ADR-0006 opens by naming the failure this avoids — a manifest referencing
`jobs-domain-golem:0.1.2` while the recipe pushed `:0.1.0`, a live broken deploy
caused by nothing but a mutable tag. `just push-tempo-ghcr` still pushes a mutable
version tag and has not been converted.

## Publishing

`.github/workflows/publish-components.yml`, and **not** on every push. `ci.yml`
opens by promising it *"does not deploy, does not publish"*, which is worth keeping,
and creating packages other people can pull should be a decision rather than a side
effect of merging. So: manual dispatch, or a `v*` tag. Turning on main-pushes is
three lines, written down in that file.

Locally, the same command:

```bash
OCI_USER=me OCI_PASSWORD="$(gh auth token)" just push-components
```

It writes `components.lock` — `<name> <digest>` per line — which is the file to pin
against, since the tags are only for typing.

## Interop, which is the reason this code still exists

ADR-0024 took OCI off the runtime path and kept `push_artifact` for one stated
reason: `wkg oci pull` interop. That claim is now checked in both directions.

Our push writes `application/wasm` layers under an
`application/vnd.wasm.config.v0+json` config — verified against
`ghcr.io/webassembly/wasi/*`, which is what `wkg` itself publishes, so the parity
test is asserting parity with current tooling and not a version that has moved on.

Pull accepts three layer media types, because the first real artifact tried was
refused by a stricter version of this code:

| media type | who writes it |
|---|---|
| `application/wasm` | `wkg`, and this repository |
| `application/vnd.wasm.content.layer.v1+wasm` | the OCI-wasm draft |
| `application/vnd.module.wasm.content.layer.v1+wasm` | older wasmCloud artifacts |

Proven end to end rather than asserted: pulling `ghcr.io/webassembly/wasi/http:0.2.0`
produces a file **byte-identical** to the one `wkg` had already cached under
`~/.cache/wasm-pkg/` on the same machine.

## Where this sits

- **ADR-0006** — the digest is the identity; a tag is human convenience. Kept.
- **ADR-0024** — artifacts reach nodes through a JetStream object store keyed by
  sha256, and a node needs no registry and no registry credential. **Unchanged.**
  This path is for people and for CI, not for the runtime.
- **ADR-0073** — public costs a signature. Not consulted here yet; see above.
