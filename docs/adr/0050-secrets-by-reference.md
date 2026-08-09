
# 0050 — Secrets, by reference

Status: accepted. Implements ADR-0010's secrets half, which had been specified for a
long time and never built.

## What existed

`secrets-vault` — a component with envelope encryption, versioning and rotation — has
been in this repo the whole time, composed into `mfa-authgate` and nothing else. The
control plane could not reach it, `manifest.rs` hardcoded `"secrets": []`, and no
endpoint stored one. Same shape as config before ADR-0047: a field with no path
behind it.

## By reference, and only by reference

```
POST   /api/secrets?org=acme   {"name":"stripe","value":"..."}  -> vault://acme/stripe
GET    /api/secrets?org=acme                                    -> names only
DELETE /api/secrets/stripe?org=acme
```

A manifest may contain `{"key":"stripe","ref":"vault://acme/stripe"}` and never a
value. Three properties make that hold rather than being a convention:

**`describe` is why this is safe.** The vault can answer "is there a secret by this
name" *without decrypting*, so a save is validated without the platform ever holding
a plaintext it has no use for. Checking existence with `get` would have meant reading
every secret on every save.

**There is no endpoint that returns a value.** The platform stores secrets so
workloads can use them, not so a browser can display them. `PUT` replies with
metadata — a caller who just wrote a secret already has it, and echoing it back puts
it in one more place.

**Names are per organisation.** One vault backs the platform, so the org is part of
the name. A shared namespace here is ADR-0012's bucket leak with a worse blast radius.

## The boundaries, measured

```
store:            {"name":"stripe","org":"acme","ref":"vault://acme/stripe","version":1}
list:             {"secrets":[{"name":"stripe","ref":"vault://acme/stripe"}]}   (no values)

a ref that does not exist
  `gate`: `vault://acme/nope` does not resolve — store it first with POST /api/secrets
another org's secret
  `gate`: `vault://globex/stripe` belongs to `globex`, and this deployment is for `acme`
a malformed ref
  `gate`: `just-a-string` is not a secret reference — it must look like `vault://acme/<name>`

does the plaintext appear in anything the platform wrote?
  not in any log or record
```

The cross-org refusal is the boundary that matters: without it a manifest could name
any secret on the platform and the vault would resolve it happily, because the vault
does not know about orgs — the naming does.

## An ordering bug this surfaced

The digest check ran before config and secrets, so a deployment with a typo'd secret
reported *"has not been distributed yet — save again in a moment"*. The author waits,
saves again, and only then learns they had a bad reference all along.

A missing digest is a **transient pipeline state**; a bad config key or unresolvable
secret is a **permanent authoring error**. The permanent ones are now reported first,
because those are the ones the author can act on right now.

## What is not done, and it is the important half

**A running component still cannot read its secrets.** The manifest carries
references, the node receives them, and nothing resolves them at runtime. Doing it in
the reconciler would be easy and wrong — the value would travel over NATS and land in
`instances.json`, which is exactly what "by reference in the manifest, the revision
and every log line" exists to prevent.

The right shape is the host resolving a reference itself at instance start, against
the platform, with its own credential — so the plaintext exists only inside the host
that needs it. That needs a host→platform path with authentication that does not
exist yet, and it is the next piece.

Until then this is real but half: secrets can be stored, referenced, validated and
audited, and cannot yet be *used*. Saying so is better than a deployment that looks
wired and hands a component nothing.

Also missing: rotation is in the vault interface and not exposed, `get-version` has no
caller, and nothing warns when a secret a live deployment references is deleted.
