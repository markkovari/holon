# Self-hosting, in three tiers

For running **your own** apps on **your own** machines. Every number here is measured
(see [ADR-0019](adr/0019-the-density-number.md), [ADR-0020](adr/0020-the-density-number-under-load.md));
every limitation is one that was hit rather than guessed.

The tiers are deliberately progressive: **one app spec, three backends.** `apps/<name>.toml`
is the only hand-authored file, and moving up a tier is an edit, not a rewrite — because
each tier is the same shape of thing, a pure function from that spec to whatever the
substrate needs. That is the property that made the Kubernetes renderer reliable, so tier 1
copies it.

| | scheduling | per component boundary | control plane per box |
|---|---|---|---|
| **tier 1** — `comp-host` + systemd + Caddy | you pick the box | **0** (fused, in-process) | **none** |
| **tier 2** — many apps per host | you pick the box | 0 (in-process) | ~200 Mi |
| **tier 3** — k3s + wasmCloud operator | declarative, cross-machine | 0 fused / ~1.2 ms linked | ~800 Mi – 1 GB |

Start at tier 1. Go up when a measurement tells you to, not before.

## The four lanes, and what each costs

One `apps/<name>.toml` renders to all four. Moving between them is an edit and a
different recipe, never a rewrite — which is the property the whole spec exists for.

| lane | what runs it | what it is for | recipe |
|---|---|---|---|
| **1. one box** | `comp-host` + systemd + Caddy | your own machine, no control plane at all | `just selfhost-deploy <app> <host>` |
| **2. a lattice** | `comp-host` per node + `comp-reconciler` + `comp-ingress` over NATS | several boxes, where *which* box is no longer your decision | `just lattice-deploy` |
| **3. wasmCloud 1.x** | wadm, driven over NATS | somebody else's wasmCloud, or the k8s operator | `just wasmcloud-deploy <app>` |
| **4. wasmCloud 2.x** | the runtime-operator, over the Kubernetes API | current wasmCloud — no wadm, no OAM | `just wasmcloud-v2-deploy <app>` |

Lanes 3 and 4 are **interop, not a recommendation.** [ADR-0021](adr/0021-there-is-no-kubernetes.md)
took Kubernetes off this platform's runtime path deliberately and priced it: 70 Mi
per pod against 2.3 Mi per extra component. They exist so a Holon app can run where
a team already runs wasmCloud, and the generated host config says so in its header.

### Triggers: HTTP is not the only one

`sched:timer`, `event:bus` and `cron:expr` are all **pull** — each says in its own
WIT that a relay must drive it, which is what keeps them pure WASI and portable.
Something outside has to poke the app, and `comp-relay` is that something in every
lane. An app declares it wants one:

```toml
[triggers]
pump = "/internal/pump"   # the convention: saga-domain and the four eshop-* services
interval = 10             # the completeness path
```

Tier 1 renders a second unit for it; on wasmCloud, `event-pusher` does the same job
through `wasmcloud:messaging`. The app's exported WIT does not change either way.

Measured rather than asserted — a `saga-domain` trip whose hotel leg fails:

| | after 6s |
|---|---|
| no relay | `running` — `[pending, pending, pending]` |
| relay, 1s interval | `compensated` — `[compensated, failed, pending]` |

### Fused or linked, and what the hop costs

Both topologies render from the same spec (`--topology`), and the choice is forced
more often than it is chosen:

- **fused** is one artifact and no hop, but wasmtime allows **30 nested instances per
  component**. The full vet-clinic is 104 core modules, so it cannot fuse — that wall
  is why the linked manifest exists. `holon wadm render` refuses an over-size fused
  render rather than letting it fail at start on the cluster.
- **linked** deploys at any size and pays a hop per boundary: **57 µs** on comp's own
  wrpc lattice ([ADR-0032](adr/0032-cross-node-invocation-and-what-the-hop-costs.md)),
  **~1.2 ms** on wasmCloud v1, where links go over NATS. Quote the microseconds, not a
  percentage — `docs/CURRENT.md` warns the ratio is not a platform property.

So the useful default for a large graph is the hybrid: fuse the pure-compute
capabilities, link only the stateful ones. `holon wadm render --topology linked` does
that on its own and prints what it fused in.

### v1 and v2 are different systems, not two versions of one manifest

This page previously implied `--api v2` was a newer envelope around the same OAM
document. It is not, and the correction is worth stating because the wrong version
**deploys cleanly and runs nothing**:

- **wasmCloud 1.x** — an OAM `Application` (`core.oam.dev/v1beta1`) submitted to
  **wadm** over NATS. Traits are `spreadscaler` / `daemonscaler` / `link`. wadm's own
  source has never used another `apiVersion`.
- **wasmCloud 2.x** — **there is no wadm and no OAM.** A `Workload`
  (`runtime.wasmcloud.dev/v1alpha1`) is applied to the **Kubernetes API** and the
  runtime-operator schedules it onto a host in a host group. `wash` 2.x has no `app`
  command at all; the host is `wash host`.

The trap: wadm ignores a trait type it does not recognise rather than refusing it.
A wrong-shaped manifest returns `"result":"acknowledged"`, creates **no scalers**,
and serves nothing. Measured against wadm 0.21. So the renderer stamps
`holon.dev/api`, and `just wasmcloud-status` says so when a deployment has no
scalers — because the cluster will not.

### What a 2.x host will and will not run

Measured from a 2.8.0 host's own startup log: it provides **standard WASI plus
`wasmcloud:messaging`, and nothing else.** There is no keyvalue backend, no
`wasi:config` store, and nothing in the `comp:` namespace — custom interfaces need
host component plugins, which release images are not built with.

Declaring an import in `hostInterfaces` is what satisfies the ones that *are*
supported; an undeclared `wasi:keyvalue` fails at link time, not at first use. So:

| app imports | 2.x |
|---|---|
| WASI only, plus declared `wasi:keyvalue` / `wasi:config` | runs |
| anything `comp:` (`comp:secrets/reader`, `comp:store/cas`) | **cannot run** — use tier 1, the lattice, or v1 |

`holon wadm render --api v2` reads the artifact and refuses the second case with the
reason, rather than letting it apply and sit at `READY=False` with the explanation
buried in a host log. Verified both ways: `lan-scanner` reached `READY=True` on
wasmCloud 2.8.0 and served; `graphviz` is refused for `comp:secrets/reader@0.1.0`.

Three more things a 2.x host refused before they were right, each now in the
renderer: the image must carry **no `oci://` prefix** (v1 requires it), a workload
must declare a **hostname** (the host routes by `Host` header on one shared port, as
`comp-ingress` does), and **every** non-standard import must be declared.

---

## Tier 1 — `comp-host` + systemd, one URL per app

One app, one process, one hostname.

```bash
just selfhost-bootstrap my-vps       # ONCE per box: install comp-host, wire Caddy
just compose-gate                    # components -> one .wasm
just selfhost-render gate            # read the unit, env file and route first
just selfhost-deploy gate my-vps     # ship it
just selfhost-status gate my-vps
```

`selfhost-bootstrap` cross-builds a **static** `comp-host` (musl, so no glibc version to
match — one binary runs on Debian, Ubuntu or Alpine), installs it, creates the
directories, appends `import /etc/caddy/comp/*.caddy` to the Caddyfile, and pins `TS_IP`.
Skipping it would install a unit pointing at a binary that is not there and drop site
files where Caddy never looks — it would appear to work and serve nothing, so
`selfhost-deploy` refuses to run until the binary exists.

For an ARM box: `just selfhost-bootstrap my-pi aarch64`.

### What each box needs

| | |
|---|---|
| ssh + sudo | the recipes are `scp` and `systemctl`, nothing more |
| tailscale, joined | for `access = "tailnet"`; `selfhost-tsip` reads its address |
| caddy | `selfhost-bootstrap` adds the import line; validates the config |
| `comp-host` | installed by `selfhost-bootstrap`, 38 MB, static |
| a DNS record per app | pointing the hostname at the box's `100.x` address (tailnet custom record or split DNS) |
| Caddy's root trusted | once per device you browse from, for `tls internal` |

The spec:

```toml
name = "gate"
access = "tailnet"                   # the default; "public" for the few things strangers need
domain = "gate.example.com"
artifact = "components/target/gate_domain.composed.wasm"
kv = "memory"                        # or redis / nats
components = ["gate-domain", "record-store", "shaper"]   # tier 3 reads these
strategy = "fused"
[config]                             # KEEP TABLES LAST (TOML: later keys join the table)
grace-period-secs = "5"
```

From that, `selfhost` renders three files: a hardened systemd unit, a `CFG_*` environment
file, and a route so the app gets its own URL over HTTPS — a Caddy site by default,
`--router traefik` or `--router tailscale-serve` if you prefer those.

**Per-app URLs.** Every app binds `127.0.0.1:<port>` and nothing else — the unit is tested
to never emit `0.0.0.0`. The proxy is the only listener that faces anything, it routes by
hostname, and it handles certificates. Ports are derived from the app name, *stably*, so a
re-render never moves a running app out from under its route; `just selfhost-check` refuses
two apps landing on the same port, domain or name, which is the one collision a single spec
cannot see.

### Reaching them: `access = "tailnet"` (the default) or `"public"`

`tailnet` renders a Caddy site with two directives that do the work:

```
gate.example.com {
	bind {$TS_IP}      # the Tailscale address ONLY — not the VPS's public interface
	tls internal       # a cert from Caddy's own CA: no ACME, no DNS provider, no record
	reverse_proxy 127.0.0.1:30386
}
```

`just selfhost-deploy` pins `TS_IP` into Caddy's unit from `tailscale ip -4` on the box.
That step is load-bearing: Caddy expands `{$TS_IP}` from its own environment, so if nothing
sets it the bind resolves to empty and Caddy listens on **every** interface — private by
intention, public in fact. `just selfhost-tsip <host>` does it alone and is idempotent.

You then need the hostname to resolve to that `100.x` address, which Tailscale can do
without any external DNS: a custom DNS record in the tailnet, or a split-DNS entry.
MagicDNS by itself gives one name per *machine*, not per app.

`tls internal` costs one thing: trusting Caddy's root on each device you browse from
(`caddy trust`, or install its `root.crt`). Worth doing rather than dropping to plain
HTTP — WireGuard already encrypts the wire, but a **secure context** is what passkeys,
service workers and the clipboard API require, and this repo has a passkey app.

**The alternatives, and why they lose here:**

| | URL | cost |
|---|---|---|
| **Caddy + `tls internal`** *(rendered)* | `https://gate.example.com` | trust one CA per device |
| `tailscale serve` (`--router tailscale-serve`) | `https://box.tailnet.ts.net/gate` | a real cert for free, but **one hostname per machine** — apps split by path, which breaks anything assuming `/` |
| one `tailscaled` per app | `https://gate.tailnet.ts.net` | the nicest names and free certs, but tens of MiB per app — it fights the density argument |
| DNS-01 with a public CA | `https://gate.example.com` | a browser-trusted cert with no CA to install, but needs a DNS provider API and a custom Caddy build |
| MagicDNS + plain HTTP | `http://box.tailnet.ts.net:30386` | nothing to set up; no secure context, ugly ports |

`--router tailscale-serve` is rendered and available if you would rather have zero
certificate work and can live with paths.

**Why nothing collides.** Nothing is shared:

| | isolated by |
|---|---|
| port | its own loopback port; the proxy routes by hostname |
| keyvalue | its own process (`memory`), or a Redis DB / NATS bucket per app |
| config | its own `EnvironmentFile` |
| state on disk | `StateDirectory=comp/<app>` — a private `/var/lib` path per unit |
| crashes, logs | its own unit; `Restart=always`, journald per app |

That is isolation by process and filesystem, which is what Unix has always done. The
platform's per-app hosts and private buses exist to defend *strangers* from each other; on
your own box you do not need them.

**Hardening**, because it serves a network either way: `DynamicUser` (a transient uid per app),
`ProtectSystem=strict`, `ProtectHome`, `NoNewPrivileges`, `PrivateTmp`, `PrivateDevices`,
`RestrictNamespaces`, `RestrictAddressFamilies`, `LockPersonality`. The one thing that
cannot be tightened is `MemoryDenyWriteExecute` — wasmtime JITs, so it needs W^X, and the
unit says so where a reader will find it.

**What tier 1 gives up:** many apps in one process. Each app costs a `comp-host` — about
**70 Mi idle, ~230 Mi once it has served traffic**. Five apps is fine on a 2 GB box; twenty
is not. `fused` still packs as many *components* as you like into one app at **2.3 Mi
each**, so you are not giving up the component model, only the sharing of one runtime
between apps.

### State: sqlite, by default

`kv = "sqlite"` is the default, and it needed to be — `Restart=always` makes restarts
routine, so a default that silently loses data is the wrong one.

One file per app, no daemon, no configuration: `comp-host` writes to
`$STATE_DIRECTORY/kv.db`, which systemd exports for any unit with `StateDirectory=`, and
which under `DynamicUser=yes` only that app's transient uid can read. So the spec says
`kv = "sqlite"` and nothing else — no path, no URL.

Proven rather than asserted: write a value, kill the process, start it again, read it
back. The same sequence under `--kv memory` returns `found: false`.

It is also **inspectable**, which is why it beat a pure-Rust embedded store:

```
$ sqlite3 /var/lib/private/comp/gate/kv.db 'select bucket, key, cast(value as text) from kv'
orders|42|paid
```

WAL journalling plus `synchronous=NORMAL`: durable across process death, readers do not
block the writer, and no fsync per commit. And `increment` runs in an IMMEDIATE
transaction, so it is **genuinely atomic** — better than the memory and NATS backends,
which read-modify-write. Verified with 8 threads × 50 increments landing on exactly 400.

The other options remain: `memory` for a pure cache, `redis` or `nats` (each needs a
`kv_url`) when apps on *different* boxes must share state.

**The limit that no backend can fix:** `wasi:keyvalue` has no compare-and-swap, so a
component doing read-then-write across two calls is still racy however strong the store
(ADR-0008). SQLite makes the host's `increment` atomic; it cannot hand a guest a
transaction.

---

## Tier 2 — many apps per host

**Not built, and the design is decided.** When per-app processes cost too much RAM, put
many apps in one runtime: **~70 Mi once, then 2.3 Mi per component**, measured, with
identical throughput and a *better* p99 than separate processes (ADR-0020).

The blocker is not the runtime, it is naming: every storage component in this catalog
hardcodes `open("default")`, so apps sharing one host would share one bucket. ADR-0012
rejected fixing that by convention **because tenant code cannot be trusted to honour it**.
Your own code can. So tier 2 is:

1. make `record-store` and its siblings read their bucket from `wasi:config` (defaulting to
   `"default"`, so nothing existing breaks);
2. give each app its own bucket name in its spec;
3. run one runtime per box with the apps linked into it.

Note the honest caveat: **one crash takes every app on that box**, and blast radius is the
thing tier 1 buys you. That is the trade, and it is why RAM pressure — not neatness —
should be what moves you.

*A correction worth recording: this cannot be done with a v2 `wash host` alone. There is no
way to place a v2 workload without the Kubernetes operator — wash 2.x has no `app`, `start`
or `link` commands, and `--user-config` is a settings file, not a workload spec. Tier 2
therefore means either `comp-host` learning to serve several artifacts behind a router
component, or accepting tier 3.*

---

## Tier 3 — k3s + the wasmCloud operator

**Built, and proven live** ([ADR-0018](adr/0018-the-platform-deploys-a-running-app.md)):
upload → push → deploy → serve, both strategies, per-app isolation, delete, drift
correction.

What it buys: **declarative placement across machines** and automatic rescheduling when a
box dies. What it costs, measured on a running cluster before any of your apps exist:

```
wasmCloud stack     320 Mi   host 170, operator 59, gateway 34, nats 32, registry 10
k8s control plane   ~500 Mi+ (apiserver, etcd, scheduler — k3s on a real VPS)
                   --------
                   ~800 Mi – 1 GB per cluster
```

Worth it when you have enough machines that deciding *where* an app runs is a chore. With
two or three, that decision is one argument to a deploy recipe.

**About wadm, corrected.** An earlier version of this page said "do not use wadm" on two
grounds. One of them survives and one does not, so both are worth stating plainly.

*The `wash` objection does not survive.* `wash app put` was indeed removed in wash 2.x,
and the manifests in `examples/vet-clinic-wasmcloud/` still instruct it. But that is a
CLI problem, not a server one: wadm's actual API is a set of NATS subjects
(`wadm.api.<lattice>.model.<verb>`) which are still served, and `tools/wadm.sh` uses
them directly. `just wasmcloud-deploy` needs no `wash` at all, and was verified against
a live wadm 0.21 + operator 0.4.0 + wasmCloud 1.6.0 — render, push, deploy, serve.

*The hop objection survives, and is the real one.* v1 links components over NATS, so
**every component boundary becomes a network hop — measured at 1.2 ms**, against 57 µs
for the same boundary on comp's own wrpc lattice. For a graph of any size that forfeits
the advantage the component model is for.

The answer is not "avoid wadm", it is **do not link what you can fuse**: render fused
when the graph fits under wasmtime's 30 nested instances, and hybrid when it does not,
so only stateful capabilities pay the hop. `holon wadm render --topology linked` fuses
the pure-compute ones automatically and prints which.

---

## Where each piece lives

| | |
|---|---|
| `apps/<name>.toml` | the app spec — the only file you write |
| `just selfhost-specs` | derive a spec for every app that has a `host-<app>` recipe and no spec yet |
| `just selfhost-bootstrap <host>` | one-time box prep: static comp-host, dirs, Caddy import, TS_IP |
| `just selfhost-deploy-all <host>` | every app in `apps/` to one box |
| `cli/src/main.rs` | tier-1 renderer, pure and tested (19 tests, incl. one that checks the flags it emits actually exist on `comp-host`). Reached as `holon node render|validate|port|ingress` |
| `tools/gen-app-specs.py` | the spec generator behind `just selfhost-specs` |
| `host/` | `comp-host` — the runtime for tiers 1 and 2 |
| `components/platform-domain/src/render.rs` | the tier-3 renderer |
| `reconciler/` | the tier-3 lane: reconcile, distribute, and `src/oci.rs` for registry push |
