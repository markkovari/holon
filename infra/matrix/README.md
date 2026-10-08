# A private Matrix server for talking to agents

Element X (iPhone, iPad) and Element (Mac) are ordinary store apps, so the
agents need no app of their own: they show up as users, in rooms, on a server
you run. This directory is that server — Synapse, loopback-only, reached from
your devices through `tailscale serve` — plus a script that imitates what the
agent bridge will post, so the whole path can be tried before the bridge exists.

Nothing here touches the agent runtime yet.

## Verified locally (arm64, Synapse 1.162.0)

* `GET /_matrix/client/versions` advertises `org.matrix.simplified_msc3575`, and
  `/_matrix/client/unstable/org.matrix.simplified_msc3575/sync` answers (401
  unauthenticated, not 404). That is the sliding sync Element X requires;
  recent Synapse has it built in, no proxy.
* A ghost user can DM a person, post text, a message with a **structured
  mention** (`m.mentions`), upload and serve a **file**, and post a **poll**.

**Not verified: how Element X on a real device behaves**, above all push when
the phone cannot reach the server. That is what `spike.sh` is for.

## Deploy

```sh
./deploy.sh local            # try it on this machine (server_name `localhost`)
./deploy.sh Malna            # an ssh host; server_name = its tailnet DNS name
./tailscale.sh Malna serve   # HTTPS (a real *.ts.net certificate), tailnet only
```

`server_name` is permanent — it is in every user id — so `deploy.sh` uses the
machine's tailnet name, which is also the URL the clients type. Secrets are
generated once on the target (`.env`, mode 0600) and never leave it. Re-running
is safe: data, keys and secrets are kept. Docker or podman, whichever exists.

Prerequisite: HTTPS certificates enabled for the tailnet (admin console → DNS) — already
the case on this tailnet.
Funnel is never used; nothing is exposed to the internet.

Undo: `ssh Malna 'sudo tailscale serve reset'`, then `docker compose down -v`
in `~/holon-matrix` and delete that directory.

## Access to the target

The scripts talk to the target over ssh. If `~/.ssh/holon_<host>` exists (for
`Malna`: `~/.ssh/holon_malna`) it is used automatically; otherwise your normal ssh
setup applies, or set `SSH_OPTS` yourself.

That key is a dedicated, passphrase-less deploy key, so automation needs no agent
or password. It is installed on the target restricted to tailnet source addresses
(`from="100.64.0.0/10,fd7a:115c:a1e0::/48"`, no agent or X11 forwarding), so it is
useless from anywhere else. To revoke it, delete its line (comment `holon-deploy`)
from `~/.ssh/authorized_keys` on the target and `~/.ssh/holon_malna*` here.

Commands are piped to `bash -s` on the target, because the login shell there may
not be bash (malna's is fish).

## Try it from your devices

```sh
REMOTE=Malna ./spike.sh https://<name>.ts.net <name>.ts.net setup   # prints YOUR password once
./spike.sh https://<name>.ts.net <name>.ts.net post    # text, mention, file, poll
./spike.sh https://<name>.ts.net <name>.ts.net loop 60 20   # a text a minute, for push
```

In Element X: sign in with the server address, user `mark`, accept the invite
from `rower`. Then check, in order:

1. Do the four messages render — mention highlighted, file openable, poll votable?
2. `loop` with Tailscale **on**: does push arrive, with content?
3. `loop` with Tailscale **off**: does a notification still arrive, and what does it say?
4. Lock the phone and wait; is delivery prompt?

## Exit node (optional)

```sh
./tailscale.sh Malna exit-node   # forwarding sysctl + advertise; then approve once in the admin console
```

**Approve it once** in the admin console (Machines → malna → Edit route settings →
Use as exit node); until then `tailscale exit-node list` shows nothing. An exit node
routes a device's *internet* traffic through that machine. It is
not needed to reach the server, which any tailnet device already can. It does
not change push.

## Files

| | |
|---|---|
| `compose.yaml` | Synapse, published on loopback only |
| `homeserver.yaml.tmpl` | private by construction: no federation, no registration, no open listener |
| `deploy.sh` | renders, generates keys and secrets once, starts, waits until healthy |
| `tailscale.sh` | `serve`, `exit-node`, `status` |
| `spike.sh` | `setup`, `post`, `loop` |
