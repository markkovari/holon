# 0073 — Public costs a signature

Status: accepted, and built. Implements ADR-0007 rule 3, which returned `501` for
as long as there was nothing to check a signature against.

## What was refused, and why that was right

```rust
if visibility == "public" {
    return Outcome::Err(501, "public requires a signed digest — signing is not implemented");
}
```

ADR-0007 said it plainly: *"`public` requires a signature. A version cannot become
public unless its digest is signed by the publisher's key… This is the one place
signing is load-bearing, so it is the one place we build it first."* An unsigned
public catalogue is worse than none — it is a supply chain with the provenance
step removed — so the honest 501 stood for eleven ADRs. This is the build.

## The shape

**A key belongs to an organisation, and only its public half is ever sent.**
`POST /api/keys` registers a SEC1 P-256 point; the platform parses it there and
then, because a key that cannot verify anything should be refused by the call
that adds it rather than by a publish six weeks later. Member, not viewer:
registering a key decides whose bytes the whole platform will later trust.

**The message signed is the digest string**, exactly as the catalogue stores it.
Signing the content address rather than a manifest is what makes the claim
checkable by anyone, later, with nothing but the bytes: the bytes *are* the
digest (ADR-0024).

P-256 because `auth-guard` already verifies ES256 with it — the platform gains no
new primitive, and it is pure Rust, which is what lets it build for
`wasm32-wasip2` at all. Both fixed-width and DER signatures are accepted, since a
signer using the `p256` crate emits one and OpenSSL emits the other, and refusing
either would be a papercut with a confusing error.

## The hole this had to close on the way

ADR-0007 rule 1 says visibility is per **version** and widens only by an explicit
act. This catalogue keys rows by `{tenant}/{id}` with no version — a gap
`CURRENT.md` already listed. Left alone, that turns rule 3 into theatre:

> sign v1 → row is public → push new bytes to the same name → the row is still
> public, and nobody signed *these* bytes.

So public is **bound to the digest it was granted for**. The row records
`signed_digest`, and a push that changes the digest demotes the row to private
with a reason. Demoted rather than refused: the upload is legitimate, it is the
public claim that is not, and re-publishing with a fresh signature is one call.

That is rule 1 held by the data instead of by a version in the key. Per-version
keys are still the better answer and still not built.

## Measured

`reconciler/tests/publish.rs` — the harness holds a real private key the platform
never sees, and signs for real:

```
a correct signature, no key registered      403
a signature over a DIFFERENT digest         403   <- sign something harmless, publish something else
somebody else's key                         403
the real thing                              200, signed_digest bound, signed_by recorded
push new bytes to the same name             the row is private again
```

The third case is the one that matters. A signature is only worth what it covers.

## What this does not do

- **No revocation.** Removing a key does not un-publish what it signed; the
  binding is to the digest, not to the key's continued existence. Deliberate for
  now — ADR-0007 rule 4 says a public digest must stay resolvable while anything
  references it — but "this key was compromised, distrust everything it signed"
  has no answer here.
- **No key rotation story** beyond registering another and re-publishing.
- **No signing helper in the CLI.** A publisher signs the digest themselves
  today, which is fine for a machine and unfriendly for a person. `comp publish`
  ought to grow a `--key` that does it.
