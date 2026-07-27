# passkey — passwordless sign-in (the phishing-resistant one)

The next rung after **AUTHGATE.md**'s TOTP. There, both sides knew a shared secret
and the user copied a 6-digit code. Here the authenticator — Touch ID, Windows
Hello, a phone, a security key — generates a key pair, **never releases the private
half**, and signs a fresh single-use challenge. Nothing to phish, nothing to reuse,
nothing to leak from the server: it stores a **public** key.

Pick a username, hit **Create a passkey**, approve the OS prompt. That's the whole
enrolment. Sign out and back in with the same prompt — or with **no username at
all**, if your authenticator keeps discoverable credentials.

![The passkey app: a “Sign in — no password” card with a username field. Typing “ada” and clicking Create a passkey signs straight in — a green shield reads “ada · signed in with a passkey” and Your passkeys lists one credential with ES256 and verified badges, “used never · counter 1”. Add another device enrols a second authenticator, so two passkeys are listed. Sign out returns to the card, and “Sign in without a username” signs back in as ada with no username typed — the credential id identified the account. A live recording of the running React app, driven by Chromium’s virtual authenticator.](docs/media/passkey.gif)

## The component (why `webauthn:verify`)

The browser does the easy part. The relying party has to do the exacting part, and
it is all parsing and cryptography with no state — which is exactly what belongs in
a component:

| check | what it stops |
|---|---|
| `clientData.type` | a registration response replayed as a login |
| `clientData.challenge` | replay of an older ceremony |
| `clientData.origin` | **a lookalike domain using the credential at all** |
| `authData.rpIdHash` | a credential for another site being presented here |
| `UP` flag | nobody was actually at the authenticator |
| `UV` flag | ...and wasn't verified by biometric/PIN, when the RP requires it |
| signature | the key that never left the authenticator signed this challenge |
| signature counter | a **cloned** authenticator (the counter went backwards) |

Skip any single one and a passkey degrades into a bearer token. The origin check is
the one that makes passkeys *unphishable* — and it is also the one an app can most
easily neuter, which brings us to the second half of the design.

```wit
register:     func(exp: expectations, client-data-json: list<u8>, attestation-object: list<u8>)
                  -> result<credential, verify-error>
authenticate: func(exp: expectations, cred: credential, client-data-json: list<u8>,
                   authenticator-data: list<u8>, signature: list<u8>)
                  -> result<assertion, verify-error>
```

Stateless: the RP issues the challenge, stores the returned `credential`, and
persists `assertion.sign-count`. `verify-error` is a **variant, not a bool** —
`origin-mismatch("http://evil.example")` and `bad-signature` mean very different
things to whoever reads the logs.

Inside: a ~100-line CBOR reader (the canonical CTAP2 subset — no new dependency),
COSE key decoding, and ES256 (`-7`) / RS256 (`-257`) verification over
`authData || sha256(clientDataJSON)` using the `p256` / `rsa` crates the repo
already builds with. WebAuthn's ES256 signatures are ASN.1 **DER**, unlike JWS's
raw `r||s` — a detail worth exactly one component to get right once.

## What the app owns

`passkey-domain` exports `wasi:http` and imports only contracts:

- **`records:store`** — accounts, and which credentials belong to whom
- **`cache:store`** — challenges: unguessable, single-use, self-expiring. Spending
  one is a `delete`, so a replay finds nothing. (A TTL cache is *exactly* the right
  shape for this; no bespoke expiry code.)
- **`session:store`** — the session a completed ceremony mints
- **`wasi:config`** — the RP ID and origin

That last one is the point: **the RP ID and origin come from config, never from the
request.** If a client could send the origin it should be checked against, the
origin check would verify nothing. `CFG_RP_ID` / `CFG_ORIGIN` are deployment facts.

One more rule the app enforces, which is not cryptography but is a real
vulnerability if you skip it: **adding a passkey to an existing account requires a
session for that account.** Otherwise "enrol my authenticator on ada's name" is a
complete takeover. Registration is open only for a username that doesn't exist yet.
And you cannot delete your last passkey — there is no password to fall back to.

## The data model

- **accounts** — `{username, user_handle, created}`. The handle is minted at
  `register/begin` and rides along with the challenge, so the account stores exactly
  the handle the authenticator was told to remember.
- **credentials** — `{id, username, public_key (COSE), alg, sign_count, aaguid,
  user_verified, backup_eligible, backed_up, attestation_format, created,
  last_used}`, indexed by `id` and `username`. `backed_up` is refreshed on every
  login: a passkey can *become* synced when it reaches a second device.

## Run it

```bash
just host-passkey   # composes, builds the SPA, serves on :3053
# open http://localhost:3053 — NOT a LAN address: WebAuthn needs a secure context,
# and http://localhost is the only plaintext origin that qualifies.

just e2e-passkey        # the ceremonies over HTTP, with a virtual authenticator
cargo test -p webauthn  # the verifier itself: 11 tests, no host
```

## The e2e is a real authenticator

`examples/passkey/tests/passkey.rs` holds a P-256 key and performs the ceremonies
itself: it builds the CBOR attestation object, the COSE public key, and DER ECDSA
signatures. The server cannot tell it from Touch ID — which is what lets the test
also produce ceremonies a real authenticator never would, and assert each one is
refused **by reason**:

- the same ceremony replayed → `400`, the challenge was already spent
- `origin: http://evil.example` → `401 origin_mismatch`
- a credential minted for `evil.example` → `401 rp_id_mismatch`
- a signature from another key, and tampered `authData` → `401 bad_signature`
- a counter that didn't advance → `401 counter_regressed`
- enrolling a second passkey without a session → `401`; with one → `201`
- a usernameless login resolving the account from the credential id → `200`
- deleting your only passkey → `409`

## What this does *not* do (and where it goes)

- **Attestation trust is not decided.** The statement's format is reported and
  `packed` self-attestation is signature-checked, but no certificate chain is
  validated and no FIDO metadata service is consulted. An RP that must restrict
  authenticator *models* should check `aaguid` against its own allow-list — that is
  policy, not verification, and the WIT says so.
- **Ed25519 (`-8`) is refused, by name.** It would need a new dependency; a
  `unsupported-algorithm(-8)` beats silently accepting nothing.
- **No conditional UI ("passkey autofill").** The username field is tagged
  `autocomplete="username webauthn"`, but the browser-mediated
  `navigator.credentials.get({mediation: "conditional"})` flow isn't wired up.
- **Sessions are bearer tokens in `localStorage`**, not cookies — fine for a demo
  on one origin, and it keeps CSRF out of the picture entirely; `session:store`
  already mints a CSRF token for the cookie version.

## Rungs left

- **Conditional mediation** — the passkey appears in the username field's autofill,
  no "Sign in" click at all.
- **Second-factor mode** — the same component verifying a passkey *after* a
  password, which is what most deployments start with (compose with `auth-guard`).
- **Attestation allow-lists** — an `aaguid` policy in `policy:guard`.
- **Cross-device** — a QR/hybrid flow is the authenticator's business, but the
  `residentKey: "preferred"` hint here already makes phone sign-in work.
- **Recovery** — today the only recovery is a second passkey. Real deployments need
  a break-glass path (an emailed magic link via `email:template` + `notify:dispatch`).
