
# 0051 — The secret reader

Status: accepted. The design stands; **the "and built" this line used to claim was
half true** — the platform half was, the host half was not, and
[ADR-0061](0061-the-secret-reader-was-never-linked.md) is the correction and the
wiring. Read the "Measured" table below with that in mind: every row in it is a fact
about the platform, and not one is about a guest.

Completes ADR-0050, which stored and validated references and said plainly that a
running component still could not read one.

## What wasmCloud does, since it is the obvious thing to copy

Their Kubernetes operator takes secrets by **name only**:

```yaml
localResources:
  environment:
    secretFrom:
      - name: db-credentials
```

> The `secretFrom` field only accepts Secret names; values are never embedded in the
> manifest.

Values arrive at the component as **environment variables** through
`wasi:cli/environment`, as plain strings, with precedence `config` → `configFrom` →
`secretFrom`. Access control is Kubernetes RBAC on the referenced Secret.

The half we already match is the important half: the manifest holds a pointer, never a
value (ADR-0050). The half worth doing differently is delivery.

## Why not environment variables here

Their host runs one workload per pod, so "the environment" is that workload's. Ours
does not: ADR-0023 put every tenant in **one process**, and ADR-0023's predecessor
deleted `build_config()` precisely because process environment in a shared process is
a cross-tenant read.

Per-`Store` WASI environments would make it *technically* safe — each instance gets its
own. The objection is not isolation, it is that "environment" is the most-dumped
namespace in computing. Crash handlers print it, `env` prints it, language runtimes log
it on startup, and every "dump your config" debugging habit finds it. A value that must
not be logged should not live in the one place everything logs.

## The design: a key, a handle, and one explicit reveal

Three moving parts, and the first is the whole security argument.

**The guest names a KEY, never a reference.** The manifest maps keys to refs; the host
holds that map; the guest asks for `"stripe"` and cannot say `vault://globex/stripe`.
This is ADR-0012's rule applied a third time — after buckets and links — and it is
mechanical, not a convention:

```rust
pub struct Scope {
    buckets: BTreeMap<String, BucketId>,      // ADR-0012
    pub links: BTreeMap<String, InstanceId>,  // ADR-0013
    secrets: BTreeMap<String, SecretRef>,     // this ADR — same shape, same reason
}
```

`SecretRef` gets the `BucketId` treatment: a newtype with a private field, mintable
only from a `Scope`, so the fetch path cannot be handed a reference that did not come
from a manifest the platform validated. The compiler enforces what prose would not.

**The value is a handle, not a string.**

```wit
interface reader {
    /// An opaque handle. Holding one is not reading one.
    resource secret {
        /// The key this handle was opened under. Safe to log — it is the name the
        /// manifest used, not the value.
        key: func() -> string;
    }

    /// `none` when this component was not granted that key. Not an error: a
    /// component may legitimately run with an optional secret absent.
    get: func(key: string) -> result<option<secret>, secret-error>;

    /// The audit point, and the only way to a plaintext.
    reveal: func(s: borrow<secret>) -> result<string, secret-error>;
}
```

The split earns its keep because `get` and `reveal` are different events. A component
that fetches a handle at startup and reveals it once per outbound call has one
legitimate pattern; a component that reveals on every request has another; and a
component that reveals a secret it never uses is worth a question. With a string
return there is one event and nothing to distinguish.

**Existence is checked at start, the value is fetched on first reveal.** ADR-0013 says
omission fails closed at start, and a missing secret discovered at 3am on the first
request is the failure that rule exists to prevent — so the host `describe`s every ref
while building the instance and refuses to start if one does not resolve. (Written in
the present tense here and not implemented until
[ADR-0061](0061-the-secret-reader-was-never-linked.md).) It does not
fetch plaintexts it may never need: a secret used on one code path should not sit in
host memory for instances that never take it.

## Getting the value to the host

The host has no platform credential today, and it must not get a general one — a node
that can read any secret is a node whose compromise is total.

**The reconciler mints a per-instance fetch token** and puts it in the start command.
It authorises exactly the refs in that instance's manifest, for a bounded time, and
nothing else. Three properties fall out:

- It grants no more than the deployment already declared, so a stolen token is worth
  what the manifest was worth.
- It is per instance, so revoking one instance's access is stopping that instance —
  something the platform can already do.
- The host stores a capability, not a secret. Losing the ledger (`instances.json`,
  ADR-0022) leaks a bounded token, not a plaintext, which is why it can keep living on
  disk.

The token is not a secret value and may be logged; the thing it fetches may not.

## Lifetime, rotation and audit

- **Cached per instance**, not per request — a secret fetched on the first reveal
  serves the rest of that instance's life. Instances are per-request (ADR-0037), so
  "per instance" is short by construction.
- **Zeroised on drop.** Cheap, and the honest thing to do with a plaintext.
- **Rotation is a restart.** A rotated secret is picked up when an instance next
  starts, which for this platform is the next request. No invalidation protocol, no
  cache coherence — the thing that makes this affordable is the same 0.43 ms start
  ADR-0040 bought.
- **Every `reveal` is audited** with tenant, app, component and key, never a value.
  `audit-log` is already composed into the platform and does exactly this.

## What this does not defend against, said plainly

**A component that is granted a secret can reveal it.** Nothing here prevents that,
and nothing can — the component needs the value to use it. Anyone reading this design
as "secrets are safe from the workload" has read it wrong.

What it does buy, precisely:

- **Cross-tenant reads are impossible**, because the guest cannot name a reference.
- **Accidental exposure is unlikely**, because a secret is not in the environment and
  not in the config map, so nothing that dumps those dumps it.
- **Every access has a record**, so "which component read this, and when" is
  answerable after a leak — which is when it is always asked.

## Measured

```
a secret, stored                        vault://dev/stripe, version 1
the platform mints a scoped token       01KZKVTPKV07…
the token fetches its granted ref       the value came back
the same token, a ref it lacks          403
no token at all                         401
an invented token                       401
plaintext on disk anywhere the
platform wrote                          none — not a log, a manifest, or the ledger
```

Two bugs surfaced on the way, both worth recording:

**The planner carries secrets as a list and the host reads them as a map.** Converting
only the non-empty case left every ordinary start sending a sequence into a field
expecting a map, and the host refused all of them — *"invalid type: sequence, expected
a map"*. Nothing served. The e2e suite caught it on the first run after the change,
which is exactly the job it was written for.

**Query values were never percent-decoded.** A reference arrived as
`vault%3A%2F%2Fdev%2Fstripe` and compared unequal to the reference it named, so a
token was told it had not been granted something it plainly had. That was a latent bug
in *every* query parameter — the market search simply had never been given a value
with a space in it.

## What is deliberately missing

- **No encryption in transit beyond the transport.** The fetch is TLS to the platform;
  there is no per-node public key wrapping the value the way wasmCloud's lattice
  secrets use xkeys. Worth adding when a node is somewhere the operator does not
  control, and not before — it protects against an attacker who can read the transport
  but not the host, which is a narrow window when the host is the thing decrypting.
- **No versions in a reference.** `vault://acme/stripe` is current-version-only, though
  the vault supports `get-version`, so a rotation with overlap cannot yet be expressed.
- **No policy on which component may hold which secret** beyond the manifest declaring
  it. `policy:guard` could carry this the way it carries catalogue visibility
  (ADR-0049), and should before a marketplace component can ask for one.
