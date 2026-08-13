# 0079 — A component forks its own app

Status: accepted; the platform half built, the guest interface still to come.
Continues [ADR-0078](0078-an-environment-is-a-derived-app.md), which made an
environment a derived app and left "a component can ask for one" open.

## The credential problem

An agent-driven loop needs the *workload* to write desired state. So the platform
has to answer "who is asking, and what may they fork?" for a call arriving from
inside a running component — where nothing the guest says can be believed.

The answer was already lying around. The reconciler mints a **fetch token** per
instance and puts it in the start command: the host holds it, the guest never
sees it, and it names the instance —
`{tenant}/{app}/{component}@{node}`. It was built for secrets (ADR-0051), but it
is really an instance's proof of identity.

So: `POST /api/internal/environments?env=…` with that token. The app to fork is
read **out of the token**, never out of a parameter. A component can fork the app
it is part of and nothing else, by construction rather than by a check someone
has to remember to write.

```
a component forks its own app   → graph-env-node-7
the same fork twice             → 409
another branch                  → graph-env-node-8
a token naming a different app  → cannot touch `graph`
no token / a forged token       → 401
```

One change was needed to make it universal: the token used to be minted **only
for instances that had secrets**, since that was all it was for. Now every
instance gets one, with an empty ref list authorising no secret.

## Which promptly broke everything

Making the mint unconditional meant every start depended on it — and a control
plane without that route (the e2e harness's stub) served **nothing at all**. Six
manifests, three of which must serve: all 503.

The fix is the semantic that should have been there from the start: **hard for
secrets, soft for identity.** An instance that was granted secrets and cannot get
a token must not start, because it would fail at its first reveal in front of a
user (ADR-0061). An instance with no secrets only wants the token to prove who it
is, so it starts without one and says so.

The general shape is worth keeping: a new optional capability that becomes a hard
dependency of the start path can take down a fleet that never asked for it.

## Still to come

- **The guest interface.** `comp:fleet/environments.spawn(name)` on the host,
  forwarding with the instance token exactly as `comp:secrets/reader.reveal`
  forwards to fetch a secret — the same async import, the same credential, the
  same "the guest names a thing, the host resolves it" shape. Today the endpoint
  is reachable by whoever holds the token, which is the host.
- **Quota.** `quota:meter` exists and is metering nothing here. A component that
  can create instances can exhaust a fleet, and the loop this is for spawns on
  purpose.
- **Expiry.** Nothing reaps an environment whose agent lost interest.
