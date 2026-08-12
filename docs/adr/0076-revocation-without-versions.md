# 0076 — Revocation without versions

Status: accepted, and built. Closes the gap
[ADR-0073](0073-public-costs-a-signature.md) named, and **declines** the
per-version catalogue keys that were supposed to be its home.

## The item said per-version keys

`CURRENT.md` has listed "no `@version` in a catalogue key" since long before
signing existed, and ADR-0073 leaned on that gap twice: rule 1 is held by binding
`public` to a signed digest rather than by a version, and revocation had "nowhere
to live".

Then I went to build it and read what the key actually is. `{tenant}/{id}` is not
only a catalogue row key. It is:

* the **blob key** for the staged wasm bytes;
* the **push-queue key** the reconciler round-trips through
  `/api/internal/pending-pushes`, `/artifact?key=` and `/pushed`;
* the deployment resolver's handle;
* the key a fused composition is stored under.

Making it `{tenant}/{id}@{version}` touches upload, blob storage, the whole push
pipeline, deployment rendering, `spec.rs` (so a manifest can say `gate@3`) and the
CLI — with the live deployment path inside the blast radius.

## What it would actually buy

Three things were claimed for it, and only one survives contact:

1. **ADR-0007 rule 1, properly.** Already safe. ADR-0073 binds `public` to the
   digest that was signed and a push demotes the row; the test proves new bytes
   cannot inherit a signature. A version in the key would be a tidier way to hold
   a property that is already held.
2. **Version history** — rollback, pinning, several versions live at once. A real
   feature, and one nobody has needed yet. A deployment already pins a digest
   (ADR-0006), so nothing is currently unable to run an old build.
3. **Somewhere for revocation to live.** This one was real, and it turned out not
   to need versions at all.

## Revocation needs provenance, not versions

A public row already records **which key vouched for it** — `signed_by`, from
ADR-0007 rule 5. That is the whole input revocation needs:

```
POST /api/keys/revoke {"name":"old"}
  → the key stops verifying anything, immediately
  → every public row in that org signed by it goes private
  → {"revoked":true,"unpublished":["doomed"],"count":1}
```

Provenance is what makes revocation actionable rather than a gesture. Recording
who signed something was worth doing for its own sake; it turns out to have been
the hard part of this too.

Three judgements, all in the test:

- **Demoted, not deleted.** ADR-0007 rule 4 says a digest anything references must
  stay resolvable. Revocation means "stop offering this to strangers", not "break
  whoever already deployed it" — a consumer who pinned the digest keeps running,
  which is what pinning is for. What they lose is the platform's word that it is
  still trusted.
- **Owner, not member.** Adding a key widens what an org may publish; revoking one
  retracts bytes that other people may be running. The louder act needs the higher
  role.
- **The key stays in the listing, marked.** A key that vanished would leave anyone
  auditing an old signature unable to find out what became of it.

Measured: two components signed by two different keys, one key revoked. The other
key's component stays public — revoking one signer must not unpublish another's
work — the revoked key stops verifying even a correct signature, and the
surviving key can re-vouch for the same bytes. Revocation distrusts a signer, not
a digest.

## So per-version keys stay unbuilt

They are now a **feature** request rather than a correctness gap: multiple live
versions of one component. When something needs that — a rollback flow, or a
publisher wanting `v2` beta while `v1` stays public — the shape is clear and the
cost is known. Until then it is a data-model migration through the deployment
path in exchange for tidiness.

`CURRENT.md` says so in those terms now, rather than listing it as something
missing that nobody can act on.
