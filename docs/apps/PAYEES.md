# payees — a payee book with IBAN-validated bank details

A directory of who you pay, where each payee's **IBAN is validated before it's
stored** — the country length *and* the ISO 7064 mod-97 checksum — by the
**`iban:validate`** component. A typo'd IBAN is refused with the reason; a valid
one is stored normalized, with its country and a grouped display form. A
**`verify`** endpoint runs the same check dry, so the SPA flags an IBAN
green/red **as you type**.

Same shape as the other showcases: one **`payees-domain`** HTTP component that
exports `wasi:http` and imports only WIT contracts — the composed **auth-guard**
(`auth:identity`) for accounts, **`records:store`** for the payees, and
**`iban:validate`** for the check. No bespoke auth, storage, or IBAN math. The
frontend is a **React + shadcn/ui** SPA (Vite + Tailwind), served by the host.

![The payees app: a New-payee form with a name and an IBAN field. Typing a tampered IBAN shows a red “checksum failed — check for a typo”; fixing it flips to a green “Valid · NL · NL91 ABNA 0417 1643 00”, and Add is enabled. Below, the payee book lists each payee with a grouped IBAN and a country badge (DE / FR / GB / NL). A live recording of the running React app.](../media/payees.gif)

## The check (why `iban:validate`)

An IBAN isn't just a string: it carries a **mod-97 checksum** and a **fixed
length per country**, and validating it is exactly the kind of small, exact logic
that's easy to get subtly wrong. `iban:validate` owns it — `validate(iban)`
normalizes (strip spaces, upper-case), checks the length against the country (for
known countries), and verifies the checksum (move the first four characters to
the end, map letters `A=10…Z=35`, require the number ≡ 1 mod 97), returning the
parsed parts or a typed reason (`bad-check`, `bad-length(got, expected)`,
`bad-country`, …).

`payees-domain` calls it on every add (a bad IBAN never reaches the store, `422`
with the reason) and on `/verify` for live UI feedback. It's pure compute, so the
same component guards a payment form, an onboarding flow, or a batch import.

## The data model

- **payees** — `{name, iban, formatted, country}`, `owner`-scoped. The stored
  `iban` is normalized (no spaces); `formatted` is grouped in fours for display.
  A fresh account is seeded a few demo payees (DE / FR / GB).

## Run it

```bash
just host-payees   # composes the component, builds the React UI, serves on :3047
# register a new account (seeded demo payees), then add a payee — the IBAN is
# validated as you type, and a typo is refused with the reason.
just e2e-payees    # /verify + add (valid stored, bad-check/length/country rejected) + ownership
```

The frontend lives in `examples/payees/ui` (Vite + React + shadcn/ui); the Add
button stays disabled until the IBAN validates.

## Rungs left

- **BBAN structure per country** — beyond length + checksum, validate the
  country-specific bank/branch/account layout.
- **SEPA credit transfer** — export a `pain.001` payment file for a set of
  payees (a `sepa:codec` component).
- **Duplicate detection** — warn when an IBAN is already in the book.
