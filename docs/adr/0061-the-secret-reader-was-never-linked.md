# 0061 — The secret reader was never linked

Status: accepted, and built — this time in the sense the word is supposed to
carry. Corrects [ADR-0051](0051-the-secret-reader.md), whose status line said
"accepted, and built" while half of what it describes did not exist.

## What was actually there

ADR-0051 designed `comp:secrets/reader` — a guest names a key, gets an opaque
handle, and `reveal` is the one audited path to a plaintext. The platform half
was real and tested: `secret_fetch`, per-instance fetch tokens, 401 for an
expired token and 403 for a reference the token does not carry, and an e2e
asserting that one org cannot read another's secret by any route.

The host half was not wired at all.

| what ADR-0051 says | what was in the tree |
|---|---|
| the guest calls `get` and `reveal` | no `impl reader::Host` anywhere |
| the host grants the interface | absent from `build_linker` and `HOST_IFACES` |
| the value is fetched on first reveal | `secrets::fetch` had **zero callers** |
| existence is checked at start | nothing checked anything |
| a component reads its secret | no component in the repo imports the reader |

Every one of those is a compiling, plausible-looking module. `secrets.rs` has a
cache that zeroises on drop, a percent-encoder, and three unit tests — and
nothing called it. A component that declared a secret was refused at start with
*"imports comp:secrets/reader, which this host cannot grant"*, so the feature was
not merely untested: it could not be used.

The ADR's "Measured" table is the tell, read back with hindsight. Every line in
it is a fact about the platform. Not one is about a guest.

## Why it was invisible

The e2e harness never passed `--platform-url` to a node. A fleet started by
`Fleet::start` therefore had no platform to fetch a secret from, so no test could
have exercised the reader even if a component had imported it. The missing wiring
and the missing harness support hid each other: the tests that existed passed,
and the ones that would have failed were unwritable.

## What is built now

- **`impl reader::Host`** — `get` resolves a guest key through `Scope::secret`,
  the third application of ADR-0012's rule after buckets and links. `reveal`
  fetches on first use, caches for the instance, and emits the audit line.
- **`reveal` is the host's one async import.** It is the only import that talks to
  the network, so it is the only one that must not block the executor thread. A
  named `imports` rule REPLACES the default rather than adding to it, and an
  unmatched rule is a compile error — a typo cannot silently leave it synchronous.
- **The interface is granted**, on both the linker and `HOST_IFACES`.
- **`?probe=1`** on the existing internal endpoint: the same authorisation,
  answered from the vault's `describe`, so a host can check that a reference
  resolves without pulling a plaintext it may never need.
- **The start-time check** ADR-0051 promised and did not do. Every granted
  reference is probed while the instance is built, and a failure refuses the
  start naming both the key and the reference. Skipped on a ledger restore, which
  is the one path required to work with no network — those references were
  checked when the instance first started, and refusing there would turn a reboot
  into an outage.
- **`secret-probe`**, an instrument, not a catalogue component. It publishes what
  it reads on purpose: ADR-0051 says plainly that a component granted a secret can
  reveal it, and this is what that sentence looks like.

## Measured

```
a granted key -> a handle              granted, and the handle carries "stripe"
the handle's key()                     the manifest's name, not the guest's string
a key that was NOT granted             none — no error, no other tenant's secret
reveal                                 sk_live_e2e, the value the vault holds
the audit line                         {"event":"secret.reveal", …,"key":"stripe"}
the value, anywhere in the node log     absent
a granted reference that does not
resolve                                 the instance never starts, and the log
                                        names the key and the reference
```

`reconciler/tests/reveal.rs`, two tests, on a real fleet.

## What this says about the other ADRs

One ADR in this tree claimed a thing was built that was half-built, and the claim
survived because the tests it would have failed could not be written. The others
are not audited by this one. The cheap check — does the capability appear in
`build_linker`, and does anything import it — is worth running against any ADR
whose "Measured" section only measures one side of a boundary.

## Still missing

Unchanged from ADR-0051 and still true: no in-transit wrapping beyond TLS and no
replay protection on the fetch, no versions in a reference, and no policy on
which component may hold which secret beyond the manifest declaring it.
