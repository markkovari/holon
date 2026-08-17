# poll — a live poll, with a browser suite (poll:domain)

Ask a question, share a link, watch the answers arrive. **One wasm component** that
serves both pages and the API, composed with four capabilities that already exist
here.

```bash
just host-poll        # → http://127.0.0.1:3057
just e2e-poll         # the Playwright suite against the real stack
```

## Everything hard is imported

| capability | what it does here |
|---|---|
| `records:store` | the polls and the votes |
| `id:generate` | the six-character share code, and the voter id |
| `svg:chart` | the results, as an `<svg>` document the page embeds |
| `qr:encode` | the share link, as a QR nobody had to draw |

So there is **no charting library on the page, no QR library, and no build step** —
`svg:chart` renders server-side and the page drops the document in. The same `<svg>`
works in a browser, an email, or a screenshot. `comp-plug` derives the whole plug
chain from the component's own imports, so adding a capability to the world does not
mean editing a recipe.

## Why this one has Playwright and `photo-critic` does not

Because the things that can break here are only visible in a browser.

**One vote per browser is a cookie rule.** A voter is a `voter=<ulid>` cookie the
component sets, and a vote record keyed by `(poll, voter)`. A single HTTP client
cannot test that: it either replays the cookie it was just handed, or never sends one
— both answers are wrong and both look like a pass. **Two browser contexts have two
cookie jars**, which is the actual claim.

**A chart is only right if the page embedded it.** `<svg>` in a response body proves
the renderer works. `<svg>` in the DOM proves the *app* works. Those are different
claims, and the second one is the one that breaks.

### Proven by breaking it

The suite was verified by sabotage, not by passing. Replacing the minted voter id
with a constant — so every browser is "the same voter" — leaves the API answering
identically, and:

```
[passed ] a poll is created, shared, and shows a QR for its link
[passed ] a question needs at least two options
[failed ] two browsers vote once each; a third vote from the same browser is refused
[passed ] the results are a server-rendered SVG in the page, with every option labelled
[passed ] an unknown code says so instead of looking broken
```

Exactly one test caught it, and it is the only one that could. Restored: 5/5 in 1.8s.

## Notes worth keeping

**The QR's URL comes from the request's own `host` header**, not from config. This app
is served through an ingress, a proxy, or a tailnet name, and a hardcoded base URL
produces a QR that works on the machine that generated it and nowhere else.

**`records::find_by` wants the JSON encoding of the value.** A code `AB12` is indexed
under `"AB12"`, quotes included, and `find_by(.., "AB12")` matches nothing while
returning `Ok(vec![])` — a wrong query and an empty collection are indistinguishable
from the caller. Hence the named `json_value` helper rather than an inline `format!`.

**The host's logs go to stderr, not stdout.** `stdio: "inherit"` in `globalSetup`
sends `comp-host: serving …` to the same stream Playwright's json reporter writes to,
and the report then starts with a log line and will not parse. Visible for debugging,
never mixed into a machine-readable stdout.

**A cookie is not identity.** It is clearable, and this is a poll rather than a
ballot. `auth-guard` is the component for the case where that matters; saying so is
cheaper than pretending a cookie is more than it is.
