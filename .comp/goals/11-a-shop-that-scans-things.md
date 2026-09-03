# A shop that scans things — 🟡 the decoder exists, the shop does not

**Status: half done.** `components/barcode-read` is written, built and gated —
the capability half of this note is finished. The SHOP half is still a note: no
domain crate, no contract, no `.toml`, nothing to run.

The decoder settled the question this note was mostly about, and the answer was
yes: a linear-barcode scanline decoder fits in `wasm32-wasip2` with no host
behind it. The built component is 179 KB and imports nothing but the `wasi:*`
boilerplate Rust's std pulls in — no capability import, no contract. So it is in
`CATALOG.md` under "Capabilities — reusable as-is", and the contract-only list
did not grow.

What it decodes, against fixtures rendered by an encoder that is not its own:
EAN-13, EAN-8, UPC-A and Code 128, upright, upside down, sideways, and with a
thumb over the bottom third. Blank paper decodes to nothing, which is the
constant-returner test. Every answer is verified against its own check digit
before it is returned.

The ceiling is stated in the crate and not hidden: a label held at 30 degrees is
not decoded. Rows and columns are scanned, each in both directions, and nothing
projects a line at an angle.

Eighteen randomly generated codes the decoder had never seen were run through it
before any of this was believed, and the first pass found a real bug —
`0166131860910` came back as `166131860910`, a leading zero stripped as if every
such code were a UPC-A. Three of those random codes are now fixtures.

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

## Why the shop half is still a note

**It is two goals wearing one name** — which is now literally true: the decoder
shipped on its own, and what is left is the application. The decoder is a capability with a
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

## What is left

- ~~A decision on whether the decoder is pure compute in-guest~~ — yes, measured.
- ~~An image fixture set with known-correct digits, checked in~~ — eleven of them,
  rotated and occluded included.
- ~~The split named~~ — the decoder is its own component and shipped alone.
- **The app's contract**, which it needs because it is decomposed: catalogue,
  basket and admin are three parts sharing one product shape.
- **The capability search asked by a person first.** Still not done, and still
  the next thing: `auth:identity` for the two audiences, `records:store` for the
  catalogue, `money:amount` for prices, `event:bus` for stock. The answer
  probably shortens the goal to "wire these together and scan a barcode", and
  writing the contract before asking is how a goal gets written for work the
  pool already does.
