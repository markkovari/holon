# contrast-audit — a WCAG contrast auditor (demo:contrastaudit)

Drop a screenshot, get the colour pairs that fail WCAG — worst first, each with a
concrete hex to replace it. It is **one wasm component** that serves the page over
HTTP and reaches Claude by egress, with the Anthropic key granted from the vault
by reference.

The same two grants `photo-critic` needs — an **external secret** and **egress** —
and the opposite payload. That one downscales a photo so a *smaller photo* can be
sent. This one **measures in the browser and sends no image at all**: the page
quantises the pixels, finds the dominant colours, and posts only the hex pairs it
found. A screenshot of an unreleased product is exactly the sort of thing people
paste into a contrast checker, and here it cannot leave the device even by
accident.

## The interesting part: the ratios are recomputed, not believed

The page computes contrast ratios to draw its own swatches, and those numbers
arrive in the request body. **They are not what gets audited.** A ratio is a claim
from outside the trust boundary, and a model handed `"ratio": 21` for two greys
would dutifully explain why that pair is fine. `src/wcag.rs` re-derives every
number from the hex pair — which is also the one thing taken on trust, and parsed
strictly.

The e2e asserts exactly this: it sends `#999999` on `#aaaaaa` **claiming 21:1**,
and the report has to be about a failure. It is — the component computes 1.23:1.

`wcag.rs` has no WASI in it, so it is host-testable, the same split
`anthropic-provider`'s `codec` uses and for the same reason: this is the part that
can be wrong in a way no integration test would notice.

```bash
cargo test -p contrast-audit --lib     # 7 tests, no fleet, no network
```

## Shape

```
components/contrast-audit/            the component: wit world + lib.rs
  src/wcag.rs                         WCAG 2.1 maths — pure, host-tested
  src/lib.rs                          serves GET / and POST /audit; does the egress
fixtures/contrast-audit.yaml          deploys it: egress api.anthropic.com:443,
                                      secret vault://acme/anthropic -> anthropic-api-key,
                                      ingress contrast.acme.test
reconciler/tests/contrast_audit_live.rs   e2e: deploy, post a palette, get a real
                                      report back over the lattice
```

## Run the e2e

One test is free and runs by default — an unauditable request must be refused
rather than answered, and it never reaches the model:

```bash
cargo test --release --test contrast_audit_live
```

The live one spends a little Anthropic credit and needs the key at
`~/.comp-secrets/anthropic`:

```bash
cargo test --release --test contrast_audit_live -- --ignored --nocapture
```

## Serve it, and put it on your phone over Tailscale

Like `photo-critic`, this app carries an external secret + egress grant, so it
serves through a deployment rather than through `just host-<app>`:

```bash
# 1. keep a fleet up with the app deployed (prints the ingress port):
comp-contrastserve

# 2. the ingress routes by Host header, so bridge it and let Tailscale do TLS:
tailscale serve --bg --https=443 http://127.0.0.1:<host-proxy-port>
#    -> https://<machine>.<tailnet>.ts.net  == the contrast auditor, on your phone
```

## Two things worth knowing

**`max_tokens` is 16000 for a report of a few hundred tokens** — nearly all
headroom, and the story behind it is a correction rather than a finding.

The app asked for 1500 and the live test failed once with `the model returned no
text` on a **200**. The obvious explanation was a thinking model spending the whole
budget before writing anything, which `goalrun`'s `--max-tokens` documents as real
(`["thinking"]`, `stop_reason: max_tokens` at 4096 on claude-sonnet-5). Raising the
budget did make it pass — so that explanation went into this README, the commit
message, and the PR.

It does not survive checking. `photo-critic` runs the same model at `max_tokens:
1024` and its live e2e passes. Setting this app back to 1500 also passes, on the
same prompt and model. So the budget was probably not the cause, the single
failure is unexplained, and raising it was a change that happened to coincide with
the problem going away.

16000 stays because `max_tokens` is a cap rather than a reservation, so headroom is
free. The part that actually earns its place is the error: an empty completion now
reports its `stop_reason` and which block types came back, so the next occurrence
names itself instead of sending someone to read the parser.

**A declared secret grant is a precondition of *starting*.** The free test first
tried to run with no secret at all, on the theory that a request refused before
egress never needs one. The host disagrees, and says so:
`cannot start: secret "anthropic-api-key" -> vault://acme/anthropic: no such
secret`, repeated until the deploy times out. The grant is checked when the
component starts, not when it reads. So the test grants a throwaway value that is
never revealed.
