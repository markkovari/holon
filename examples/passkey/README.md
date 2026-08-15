# passkey — passwordless sign-in (e2e + SPA)

See **[docs/apps/PASSKEY.md](../../docs/apps/PASSKEY.md)** for what this is and why.
`tools/screencast/passkey.mjs` records its gif with Chromium's CDP virtual
authenticator — the same trick works for driving passkeys in your own browser
tests, no Touch ID needed.

```
tests/passkey.rs   the e2e — a VIRTUAL AUTHENTICATOR (a real P-256 key) performing
                   the real ceremonies over HTTP, including the ones that must fail
ui/                the React + shadcn SPA (Vite + Tailwind) -> dist/
```

```bash
just host-passkey       # SPA + host on :3053
just e2e-passkey        # both ceremonies + every check that must bite
cargo test -p webauthn  # the verifier itself (from components/)
```

Open **http://localhost:3053** — not a LAN address. WebAuthn requires a secure
context, and `http://localhost` is the only plaintext origin that qualifies; the
RP ID (`localhost`) must match the page you loaded, or the browser refuses the
ceremony before the server ever sees it.
