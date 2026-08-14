# photo-critic — an AI photo critique (demo:photocritic)

Upload a photo, get an honest read: **interesting / composition / what to
change**. It is **one wasm component** that serves the upload UI over HTTP and
reaches Claude's **vision** API by egress, with the Anthropic key granted from
the vault by reference. The browser downscales the image (high-quality,
step-wise, long edge ≤ 1568px, JPEG) before upload, so nothing large crosses the
wire or hits the API.

Unlike most examples here — self-contained, records-only — this one needs the two
grants a vision app requires: an **external secret** and **egress**. That is the
interesting part: it is the `anthropic-provider` egress+secret pattern, but
serving HTTP and sending an image content block.

## Shape

```
components/photo-critic/            the component: wit world + lib.rs
                                    (serves GET / and POST /evaluate; does the vision egress)
fixtures/photo-critic.yaml          deploys it: egress api.anthropic.com:443,
                                    secret vault://acme/anthropic -> anthropic-api-key,
                                    ingress photo.acme.test
reconciler/tests/photo_critic_live.rs   e2e: deploy, upload an image, get a real
                                    critique back over the lattice
```

## Run the e2e (spends a little Anthropic credit)

Needs the key at `~/.comp-secrets/anthropic`.

```bash
cargo test --release --test photo_critic_live -- --ignored --nocapture
```

The UI serves over the lattice, and `/evaluate` returns a real critique — the
component reached over egress with the key from the vault, answered over NATS.

## Serve it, and put it on your phone over Tailscale

The single-`comp-host` serve the records-only examples use (`just host-<app>`)
does not carry an external secret + egress grant, so this app serves through a
deployment:

```bash
# 1. keep a fleet up with the app deployed (prints the ingress port):
comp-photoserve

# 2. the ingress routes by Host header, so bridge it and let Tailscale do TLS:
#    (any reverse proxy that sets Host: photo.acme.test works)
tailscale serve --bg --https=443 http://127.0.0.1:<host-proxy-port>
#    -> https://<machine>.<tailnet>.ts.net  == the photo critic, on your phone
```

Tailnet-only, TLS by Tailscale. Open it, upload a photo, get the read.
