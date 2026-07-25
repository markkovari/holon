# payees — a payee book with IBAN-validated bank details (PAYEES.md)

A directory of who you pay, where each payee's **IBAN is validated** (country
length + ISO 7064 mod-97 checksum) by the **`iban:validate`** component before
it's stored. A `/verify` endpoint runs the same check dry, so the SPA flags an
IBAN green/red as you type. See [PAYEES.md](../../PAYEES.md).

A composed HTTP app on the native Rust host, with a **React + shadcn/ui** SPA.

```
ui/                      # Vite + React + TS + Tailwind + shadcn/ui source
public/ -> (built)       # `npm run build` emits ../dist, which the host serves
tests/payees.rs          # e2e: /verify + add (valid/bad) + ownership
```

## Run

```bash
# from the repo root:
just host-payees         # composes the component + builds the UI + serves on :3047
```

Open `http://127.0.0.1:3047`: **register** a new account — you get a few demo
payees. Add a payee; the IBAN field validates live (a typo shows the reason), and
Add stays disabled until it's valid.

```bash
just e2e-payees          # IBAN validation + the payee book
# work on the UI live:
cd examples/payees/ui && npm install && npm run dev
```
