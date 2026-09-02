# A shop that scans things — 🔴 TODO, nothing written yet

**Status: a note, not a worklist item.** No component, no contract, no gates, no
`.toml`. Written down so the idea stops living in a chat window; deliberately not
started. Do not run this — there is nothing to run.

## What is wanted

A barcode-reading capability, and a two-audience grocery app on top of it.

**1. `barcode:read` — a new capability component.**

Decode a barcode from an image into its digits and its symbology. The formats a
grocery app actually meets: EAN-13 and EAN-8 (Europe), UPC-A (imports), and
Code-128 for shelf and price labels. QR is a different problem and can wait for a
reason to exist.

The interesting half is where it can live. `pii-redact`, `geo` and `csv` are pure
compute and therefore real components; a decoder is pure compute too — bytes in,
digits out — so unlike `browser-automation` or `lan-scanner` this does **not** have
to be a contract with a host behind it. That is the thing worth checking first:
whether a linear-barcode scanline decoder fits in `wasm32-wasip2` with no imports
beyond the image bytes. If it does, it belongs in `components/` with the others and
`docs/CURRENT.md`'s twelve-contracts-with-nothing-behind-them list does not grow.

What it must not become: a component that returns `"4006381333931"` for every input.
Three of the contract-only capabilities shipped a plausible constant
(`"mocked_clipboard_text_123"`, `"wg0 is UP, 2 peers connected"`) and that is
recorded as worse than returning nothing, because no caller and no reader of the
catalogue could tell them apart from something that works. Either it decodes a real
image in a fixture, or it exports `UNIMPLEMENTED:` and says so in `CATALOG.md`.

**2. A grocery app with two audiences, end to end.**

- **admin** — the catalogue: add a product by scanning it, set a price, adjust
  stock, see what is running out.
- **user** — the shopper: scan to look a product up, a basket, a total.

One `apps/<name>.toml`, both audiences behind one ingress, delivered through one of
the four lanes in `docs/SELFHOST.md`.

## Why it is 🔴 and staying there

Two reasons, and neither is the barcode.

**It is two goals wearing one name.** The decoder is a capability with a
deterministic gate — an image fixture in, known digits out, and a wrong decoder
fails loudly. The shop is an application with auth, roles, money and stock, and
`eshop-basket`, `eshop-catalog`, `eshop-ordering` and `money` already exist to build
it out of. Those want different gates and probably different runs. Splitting them is
the first real work.

**The shop half is mostly already in the pool.** `auth:identity` for the two
audiences, `records:store` for the catalogue, `money:amount` for prices,
`event:bus` for stock changes. Before any of it is written, the capability search
(ADR-0094) should be asked what it answers for "a grocery catalogue with an admin
and a shopper" — that question is now asked automatically on every run including
decomposed ones, and here it should be asked by a person first, because the answer
probably shortens the goal to "wire these together and scan a barcode".

## What would make it 🟡

- A decision on whether the decoder is pure compute in-guest, with a spike that
  either decodes one fixture image or says why it cannot.
- An image fixture set with known-correct digits, checked in — including a rotated
  and a partially-occluded one, because a decoder that only reads a clean render is
  a decoder that works in the test and not in a shop.
- The split named: one goal for `barcode:read`, one for the app.
- The app's contract written, if it is decomposed — which it probably is: catalogue,
  basket and admin are three parts that share a product shape.
