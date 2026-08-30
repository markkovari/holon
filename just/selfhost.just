# --------------------------------------------------------------------------
# Tier 1: your own machine, no control plane.
#
# `comp-host` + systemd + Caddy, rendered from the same `apps/<name>.toml` the
# other three lanes read. See docs/SELFHOST.md for what each lane costs.
# --------------------------------------------------------------------------

# Cross-build a STATIC comp-host for a Linux box.
#
# musl on purpose: the result has no glibc version to match, so one binary runs on
# Debian, Ubuntu or Alpine. Needs `cross` and a running docker/orbstack. Takes ~4
# minutes cold, because wasmtime.
#
#   just selfhost-build-host              # x86_64 (most VPSes)
#   just selfhost-build-host aarch64      # ARM boxes, Pi, Hetzner/Oracle ARM
selfhost-build-host arch="x86_64":
    cd host && cross build --release --target {{arch}}-unknown-linux-musl
    @file host/target/{{arch}}-unknown-linux-musl/release/comp-host
    @ls -la host/target/{{arch}}-unknown-linux-musl/release/comp-host | awk '{printf "  %.0f MB\n", $5/1048576}'

# Cross-build a STATIC comp-relay — what makes a pull-based timer or topic fire.
#
# Small next to comp-host: it is an HTTP client with a clock and JITs nothing.
selfhost-build-relay arch="x86_64":
    cd reconciler && cross build --release --target {{arch}}-unknown-linux-musl --bin comp-relay
    @ls -la reconciler/target/{{arch}}-unknown-linux-musl/release/comp-relay | awk '{printf "  comp-relay %.0f MB\n", $5/1048576}'

# ONE-TIME per box. Installs the runtime and makes Caddy read what this lane writes.
#
# Every remote command goes through `bash -lc`. `ssh host "…"` hands the string to
# the remote LOGIN shell, and there is no rule that it is a POSIX one: the first box
# this lane met that did not have sh as a login shell answered
# `fish: Missing end to balance this if statement` and stopped, having already
# copied the binaries. The lane needs bash on the box, which it needed anyway.
#
# Without this, `selfhost-deploy` would install a unit pointing at a comp-host that
# does not exist, and drop site files into a directory Caddy never reads — so it would
# look like it worked and serve nothing.
selfhost-bootstrap host arch="x86_64": (selfhost-build-host arch) (selfhost-build-relay arch)
    #!/usr/bin/env bash
    set -euo pipefail
    BIN=host/target/{{arch}}-unknown-linux-musl/release/comp-host
    scp "$BIN" {{host}}:/tmp/comp-host
    # comp-relay too: an app whose spec declares [triggers] gets a second unit, and a
    # unit pointing at a binary that is not there fails at start rather than at deploy.
    RELAY=reconciler/target/{{arch}}-unknown-linux-musl/release/comp-relay
    if [ -f "$RELAY" ]; then scp "$RELAY" {{host}}:/tmp/comp-relay; fi
    # `ssh host "script"` runs the string in the remote LOGIN shell, and nothing
    # promises that is a POSIX one. The first box this lane met whose login shell
    # was fish answered `Missing end to balance this if statement` and stopped —
    # after the binaries had already been copied. `ssh host bash -s` with the script
    # on STDIN sidesteps the login shell's parser entirely, and needs no quoting.
    ssh {{host}} bash -s <<'REMOTE'
    set -e
    sudo install -m 0755 /tmp/comp-host /usr/local/bin/comp-host; rm -f /tmp/comp-host
    if [ -f /tmp/comp-relay ]; then
      sudo install -m 0755 /tmp/comp-relay /usr/local/bin/comp-relay; rm -f /tmp/comp-relay
    fi
    sudo mkdir -p /srv/comp /etc/comp /etc/caddy/comp
    sudo chmod 0711 /etc/comp
    /usr/local/bin/comp-host --help >/dev/null && echo '  comp-host installed:' \
      && /usr/local/bin/comp-host --help | head -1
    REMOTE
    # Caddy only reads files it is told to, and an import line at the end of the
    # Caddyfile is top-level — where site blocks belong. Skipped entirely when Caddy
    # is not installed: `--router tailscale-serve` needs no proxy at all, and a box
    # routed that way should not be made to grow one.
    ssh {{host}} bash -s <<'REMOTE'
    set -e
    command -v caddy >/dev/null || { echo "  no caddy — fine for --router tailscale-serve"; exit 0; }
    sudo touch /etc/caddy/Caddyfile
    grep -qF 'import /etc/caddy/comp/*.caddy' /etc/caddy/Caddyfile \
      || echo 'import /etc/caddy/comp/*.caddy' | sudo tee -a /etc/caddy/Caddyfile >/dev/null
    sudo caddy validate --config /etc/caddy/Caddyfile 2>&1 | tail -2 || true
    REMOTE
    just selfhost-tsip {{host}}
    # Not backticks. This is a bash recipe, and `just selfhost-deploy <app> {{host}}`
    # in backticks is a command substitution — it RAN, with `<app>` as a redirect.
    echo "  bootstrapped {{host}} — 'just selfhost-deploy <app> {{host}}' will work now"

# Cross-build the agent for a box. Same target as the host it sits beside.
selfhost-build-agent arch="x86_64":
    cd reconciler && cross build --release --target {{arch}}-unknown-linux-musl --bin comp-agent
    @ls -la reconciler/target/{{arch}}-unknown-linux-musl/release/comp-agent | awk '{printf "  comp-agent %.0f MB\n", $5/1048576}'

# Install the agent and its timer on a box, and tell it which lock to read.
#
# ONE-TIME, like `selfhost-bootstrap`. The agent updates apps that already have a
# unit and installs nothing new — so `selfhost-deploy` stays the deliberate act of
# putting an app on a machine, and this keeps what is there current.
selfhost-agent host arch="x86_64" interval="300": (selfhost-build-agent arch)
    #!/usr/bin/env bash
    set -euo pipefail
    OWNER="${OWNER:-markkovari}"
    LOCK="${LOCK_URL:-https://raw.githubusercontent.com/$OWNER/holon/deploy/apps.lock}"
    scp reconciler/target/{{arch}}-unknown-linux-musl/release/comp-agent {{host}}:/tmp/comp-agent
    # comp-oci does the pulling, so it has to be there too.
    cd reconciler && cross build --release --target {{arch}}-unknown-linux-musl --bin comp-oci && cd ..
    scp reconciler/target/{{arch}}-unknown-linux-musl/release/comp-oci {{host}}:/tmp/comp-oci
    { echo "LOCK='$LOCK'"; echo "EVERY='{{interval}}'"; cat; } <<'REMOTE' | ssh {{host}} bash -s
    set -e
    sudo install -m 0755 /tmp/comp-agent /usr/local/bin/comp-agent
    sudo install -m 0755 /tmp/comp-oci /usr/local/bin/comp-oci
    rm -f /tmp/comp-agent /tmp/comp-oci
    # A timer and a oneshot, not a daemon with its own sleep: systemd already owns
    # "run this every N seconds", it survives reboots, and `systemctl list-timers`
    # answers "when does it next run" without reading anyone's source.
    printf '%s\n' \
      '[Unit]' \
      'Description=comp-agent: keep this box'"'"'s apps at the published digest' \
      'After=network-online.target' \
      'Wants=network-online.target' \
      '' \
      '[Service]' \
      'Type=oneshot' \
      "ExecStart=/usr/local/bin/comp-agent --lock-url $LOCK --once" \
      | sudo tee /etc/systemd/system/comp-agent.service >/dev/null
    printf '%s\n' \
      '[Unit]' \
      'Description=comp-agent every '"$EVERY"'s' \
      '' \
      '[Timer]' \
      'OnBootSec=60' \
      "OnUnitActiveSec=${EVERY}" \
      'AccuracySec=10' \
      '' \
      '[Install]' \
      'WantedBy=timers.target' \
      | sudo tee /etc/systemd/system/comp-agent.timer >/dev/null
    sudo systemctl daemon-reload
    sudo systemctl enable --now comp-agent.timer
    echo "  agent installed; next run:"
    systemctl list-timers comp-agent.timer --no-pager | sed -n 2p
    REMOTE
    echo "  {{host}} follows $LOCK every {{interval}}s"

# What the agent WOULD do, changing nothing. Run this first on a box.
selfhost-agent-dry host:
    #!/usr/bin/env bash
    # A shebang, or `just` runs each line as its own command and the heredoc never
    # reaches ssh — `systemctl` then runs on the laptop, which does not have one.
    set -euo pipefail
    ssh {{host}} bash -s <<'REMOTE'
    OWNER="${OWNER:-markkovari}"
    /usr/local/bin/comp-agent --once --dry-run \
      --lock-url "https://raw.githubusercontent.com/$OWNER/holon/deploy/apps.lock"
    REMOTE

# Is it running, and what did it last do?
selfhost-agent-status host:
    #!/usr/bin/env bash
    set -uo pipefail
    ssh {{host}} bash -s <<'REMOTE'
    systemctl list-timers comp-agent.timer --no-pager | head -3
    echo "--- last run:"
    journalctl -u comp-agent.service -n 15 --no-pager
    echo "--- installed digests:"
    for d in /srv/comp/*/digest; do [ -f "$d" ] && echo "  $(basename "$(dirname "$d")") $(cat "$d")"; done
    REMOTE

# Install SurrealDB on a box, bound to its TAILNET address only.
#
# Not 0.0.0.0: a database with a root credential should not be in front of every
# device on whatever LAN the box happens to sit on. The password is generated here,
# written to /etc/surrealdb/root.pass with 0600, and read by the unit at start — so
# it is never in `ExecStart`, and therefore never in `ps` for every user on the box.
#
#   just selfhost-surreal malna
#   SURREAL_URL=http://malna:8000 SURREAL_PASS=$(cat ~/.comp-secrets/surreal) just capgraph-store
selfhost-surreal host version="v3.1.3":
    #!/usr/bin/env bash
    set -euo pipefail
    ts=$(ssh {{host}} tailscale ip -4 2>/dev/null | head -1)
    [ -n "$ts" ] || { echo "{{host}} has no tailscale address — this binds to the tailnet and there is none" >&2; exit 1; }
    arch=$(ssh {{host}} uname -m)
    case "$arch" in
      aarch64|arm64) rel=linux-arm64 ;;
      x86_64)        rel=linux-amd64 ;;
      *) echo "no SurrealDB build named for $arch" >&2; exit 1 ;;
    esac
    # Generated here so it never travels through a shell history or an argument.
    pass=$(openssl rand -base64 24 | tr -d '/+=' | head -c 28)
    { echo "TS='$ts'"; echo "REL='$rel'"; echo "VER='{{version}}'"; echo "PASS='$pass'"; cat; } <<'REMOTE' | ssh {{host}} bash -s
    set -e
    cd /tmp
    curl -fsSL -o surreal.tgz "https://github.com/surrealdb/surrealdb/releases/download/${VER}/surreal-${VER}.${REL}.tgz"
    tar xzf surreal.tgz && sudo install -m 0755 surreal /usr/local/bin/surreal && rm -f surreal.tgz surreal
    sudo mkdir -p /var/lib/surrealdb /etc/surrealdb && sudo chmod 0700 /etc/surrealdb
    printf '%s' "$PASS" | sudo tee /etc/surrealdb/root.pass >/dev/null
    sudo chmod 0600 /etc/surrealdb/root.pass
    sudo tee /etc/systemd/system/surrealdb.service >/dev/null <<UNIT
    [Unit]
    Description=SurrealDB — the derived graph, and the pool that cannot be recomputed
    After=network-online.target tailscaled.service
    Wants=network-online.target

    [Service]
    Type=simple
    ExecStart=/bin/sh -c '/usr/local/bin/surreal start --bind ${TS}:8000 --user root --pass "\$(cat /etc/surrealdb/root.pass)" rocksdb:/var/lib/surrealdb/data'
    Restart=always
    RestartSec=3
    NoNewPrivileges=yes
    ProtectSystem=strict
    ProtectHome=yes
    ReadWritePaths=/var/lib/surrealdb
    PrivateTmp=yes

    [Install]
    WantedBy=multi-user.target
    UNIT
    sudo systemctl daemon-reload
    sudo systemctl enable --now surrealdb
    sleep 4
    systemctl is-active surrealdb
    REMOTE
    echo "$pass" > ~/.comp-secrets/surreal
    chmod 600 ~/.comp-secrets/surreal
    echo "  surreal on {{host}} at http://{{host}}:8000 (tailnet only)"
    echo "  password in ~/.comp-secrets/surreal — by reference, never in a profile"

# Is it up, and what is in it?
selfhost-surreal-status host:
    #!/usr/bin/env bash
    set -uo pipefail
    ssh {{host}} bash -s <<'REMOTE'
    systemctl is-active surrealdb
    systemctl show surrealdb -p ActiveEnterTimestamp --value
    REMOTE
    pass=$(cat ~/.comp-secrets/surreal)
    curl -sS -u "root:$pass" -H 'Accept: application/json' \
      -H 'surreal-ns: comp' -H 'surreal-db: goalmemory' \
      --data-binary 'SELECT count() FROM interface GROUP ALL; SELECT count() FROM app GROUP ALL;' \
      "http://{{host}}:8000/sql" | python3 -c 'import sys,json;d=json.load(sys.stdin);print("  interfaces:", (d[0]["result"] or [{}])[0].get("count", 0));print("  apps:      ", (d[1]["result"] or [{}])[0].get("count", 0))'

# Render one app's systemd unit, env file and route WITHOUT touching a box.
# Read the output before you trust it to a server.
selfhost-render app router="caddy": build-selfhost
    ./cli/target/release/holon node render apps/{{app}}.toml \
      --out target/selfhost --router {{router}}
    @echo "--- unit ---";  cat target/selfhost/{{app}}/comp-{{app}}.service
    @echo "--- route ---"; cat target/selfhost/{{app}}/{{app}}.*

# Derive a spec for every app that has a `host-<app>` recipe but no `apps/*.toml`.
#
# The recipe already names the artifact, the port and the SPA directory — a spec is
# those three facts plus a hostname. What the generator cannot know it does not
# guess: `domain` is a placeholder to edit, and `access` is left at its
# fail-closed default. An existing spec is never overwritten.
selfhost-specs:
    python3 tools/gen-app-specs.py
    @just selfhost-check

# Refuse the collisions a single spec cannot see: two apps on one port, one
# domain, or one name. Run it in CI over apps/*.toml.
selfhost-check: build-selfhost
    ./cli/target/release/holon node validate apps/*.toml

# Ship one app to one box: compose, render, copy, restart, route.
#
# Needs on the box: comp-host at /usr/local/bin, caddy (or traefik), and an ssh
# key. It is deliberately plain scp+systemctl — for a handful of machines a
# control plane costs more than it saves (docs/SELFHOST.md).
selfhost-deploy app host router="caddy": build-selfhost
    #!/usr/bin/env bash
    set -euo pipefail
    ART=$(python3 -c "import tomllib,sys;print(tomllib.load(open('apps/{{app}}.toml','rb'))['artifact'])")
    if [ ! -f "$ART" ]; then
      echo "missing $ART — run the app's compose recipe first (just compose-{{app}})" >&2
      exit 1
    fi
    ./cli/target/release/holon node validate apps/*.toml
    # Fail here, not with a unit that cannot start. The binary is the one thing the
    # deploy does NOT ship — it is 38 MB and identical for every app on the box.
    if ! ssh {{host}} "test -x /usr/local/bin/comp-host"; then
      echo "{{host}} has no /usr/local/bin/comp-host — run: just selfhost-bootstrap {{host}}" >&2
      exit 1
    fi
    ./cli/target/release/holon node render apps/{{app}}.toml --out target/selfhost --router {{router}}
    D=target/selfhost/{{app}}
    # 0644 on the artifact: the unit runs under DynamicUser, a transient uid that
    # must still be able to read it. 0600 on the env file: it may hold secrets and
    # systemd reads it as root before dropping privileges.
    ssh {{host}} bash -s <<'REMOTE'
    sudo mkdir -p /srv/comp/{{app}} /etc/comp
    command -v caddy >/dev/null && sudo mkdir -p /etc/caddy/comp
    exit 0
    REMOTE
    scp "$ART" {{host}}:/tmp/{{app}}.wasm
    scp "$D/comp-{{app}}.service" {{host}}:/tmp/
    scp "$D/{{app}}.env" {{host}}:/tmp/
    # Rendered only when the spec declares [triggers].
    if [ -f "$D/comp-{{app}}-relay.service" ]; then scp "$D/comp-{{app}}-relay.service" {{host}}:/tmp/; fi
    # The SPA, if the spec names one. The renderer already points the unit at
    # /srv/comp/<app>/static and nothing was ever putting anything there — the app
    # served its API perfectly and answered 404 at `/`, which is the most confusing
    # shape a deploy failure can take. Shipped as a tar so one scp carries a tree,
    # and replaced wholesale so a file deleted from the SPA does not survive on the
    # box forever.
    STATIC=$(python3 -c "import tomllib;print(tomllib.load(open('apps/{{app}}.toml','rb')).get('static_dir',''))")
    if [ -n "$STATIC" ]; then
      [ -d "$STATIC" ] || { echo "spec names static_dir '$STATIC' which does not exist — run the app's build-*-ui recipe" >&2; exit 1; }
      tar -czf "$D/{{app}}-static.tgz" -C "$STATIC" .
      scp "$D/{{app}}-static.tgz" {{host}}:/tmp/
    fi
    # One of three, whichever the router rendered. `.serve.sh` was missing from this
    # list while the install block below already knew how to place it — so
    # `--router tailscale-serve` rendered fine and could never be deployed, failing
    # on `stat local "…/events.yml"` after the wasm was already on the box.
    for ext in caddy yml serve.sh; do
      if [ -f "$D/{{app}}.$ext" ]; then scp "$D/{{app}}.$ext" {{host}}:/tmp/; fi
    done
    # On STDIN, not as an argument: `ssh host "…"` is parsed by the remote LOGIN
    # shell, and a box whose login shell is fish rejects `if … then … fi` outright.
    ssh {{host}} bash -s <<'REMOTE'
    set -e
    sudo install -m 0644 /tmp/{{app}}.wasm /srv/comp/{{app}}/app.wasm
    if [ -f /tmp/{{app}}-static.tgz ]; then
      sudo rm -rf /srv/comp/{{app}}/static
      sudo mkdir -p /srv/comp/{{app}}/static
      sudo tar -xzf /tmp/{{app}}-static.tgz -C /srv/comp/{{app}}/static
      sudo chmod -R a+rX /srv/comp/{{app}}/static
    fi
    sudo install -m 0600 /tmp/{{app}}.env /etc/comp/{{app}}.env
    sudo install -m 0644 /tmp/comp-{{app}}.service /etc/systemd/system/comp-{{app}}.service
    if [ -f /tmp/comp-{{app}}-relay.service ]; then
      sudo install -m 0644 /tmp/comp-{{app}}-relay.service /etc/systemd/system/comp-{{app}}-relay.service
    fi
    if [ -f /tmp/{{app}}.serve.sh ]; then
      sudo install -m 0755 /tmp/{{app}}.serve.sh /srv/comp/{{app}}/serve.sh
    elif [ -f /tmp/{{app}}.caddy ]; then
      sudo install -m 0644 /tmp/{{app}}.caddy /etc/caddy/comp/{{app}}.caddy
    elif [ -f /tmp/{{app}}.yml ]; then
      sudo install -m 0644 /tmp/{{app}}.yml /etc/traefik/comp/{{app}}.yml
    fi
    sudo systemctl daemon-reload
    sudo systemctl enable --now comp-{{app}}
    sudo systemctl restart comp-{{app}}
    if [ -f /etc/systemd/system/comp-{{app}}-relay.service ]; then
      sudo systemctl enable --now comp-{{app}}-relay
      sudo systemctl restart comp-{{app}}-relay
    fi
    if [ -f /srv/comp/{{app}}/serve.sh ]; then sudo /srv/comp/{{app}}/serve.sh; fi
    rm -f /tmp/{{app}}.wasm /tmp/comp-{{app}}.service /tmp/comp-{{app}}-relay.service \
          /tmp/{{app}}.env /tmp/{{app}}.caddy /tmp/{{app}}.yml /tmp/{{app}}.serve.sh \
          /tmp/{{app}}-static.tgz
    REMOTE
    # Only a tailnet app needs TS_IP. Pinning it for a public app would report a
    # missing tailscale on a box that has no reason to have one, which reads as a
    # failed deploy when nothing is wrong.
    ACCESS=$(python3 -c "import tomllib;print(tomllib.load(open('apps/{{app}}.toml','rb')).get('access','tailnet'))")
    if [ "$ACCESS" = "tailnet" ]; then just selfhost-tsip {{host}}; fi
    # Only if there is a Caddy to reload. A box routed by `tailscale serve` has none,
    # and `systemctl reload caddy` on it prints a failure for something that is
    # working — which reads as a broken deploy.
    ssh {{host}} bash -s <<'REMOTE'
    command -v caddy >/dev/null || exit 0
    sudo systemctl reload caddy 2>/dev/null || sudo systemctl restart caddy 2>/dev/null || true
    REMOTE
    # Not `@echo`: this is a bash recipe, and just's `@` prefix is only meaningful
    # on a line just itself runs.
    echo "deployed {{app}} to {{host}} [$ACCESS]"

# Pin Caddy's TS_IP to this box's Tailscale address, so that `bind {$TS_IP}` in a
# tailnet route means what it says.
#
# This matters more than it looks: Caddy substitutes {$TS_IP} from its OWN
# environment, so if nothing sets it the bind resolves to empty and Caddy listens on
# EVERY interface — the exact opposite of private, silently. Idempotent; on a box with
# no tailscale it reports that and changes nothing.
selfhost-tsip host:
    #!/usr/bin/env bash
    set -uo pipefail
    IP=$(ssh {{host}} "tailscale ip -4 2>/dev/null | head -1" || true)
    if [ -z "$IP" ]; then
      echo "  {{host}}: no tailscale address — a tailnet route here would bind nothing." >&2
      echo "  Install tailscale, or set access = \"public\" for apps on this box." >&2
      exit 0
    fi
    # Only if Caddy is there. This pins TS_IP into Caddy's unit so a tailnet route
    # binds the 100.x address and nothing else; a box routed by `tailscale serve`
    # has no Caddy and needs none, and writing a drop-in for an absent unit would
    # be a daemon-reload that means nothing.
    { echo "TS_ADDR='$IP'"; cat; } <<'REMOTE' | ssh {{host}} bash -s
    set -e
    command -v caddy >/dev/null || exit 0
    sudo mkdir -p /etc/systemd/system/caddy.service.d
    printf '[Service]\nEnvironment=TS_IP=%s\n' "$TS_ADDR" \
      | sudo tee /etc/systemd/system/caddy.service.d/ts-ip.conf >/dev/null
    sudo systemctl daemon-reload
    REMOTE
    echo "  {{host}}: tailscale address $IP"

# Is it up, and what is it doing?
selfhost-status app host:
    ssh {{host}} "systemctl status comp-{{app}} --no-pager -n 15 || true"

# Remove an app from a box, including its state.
selfhost-remove app host:
    ssh {{host}} "set -e; \
      sudo systemctl disable --now comp-{{app}}-relay || true; \
      sudo systemctl disable --now comp-{{app}} || true; \
      sudo rm -f /etc/systemd/system/comp-{{app}}-relay.service \
                 /etc/systemd/system/comp-{{app}}.service /etc/comp/{{app}}.env \
                 /etc/caddy/comp/{{app}}.caddy /etc/traefik/comp/{{app}}.yml; \
      sudo rm -rf /srv/comp/{{app}} /var/lib/private/comp/{{app}} /var/lib/comp/{{app}}; \
      sudo systemctl daemon-reload; sudo systemctl reload caddy 2>/dev/null || true"
    @echo "removed {{app}} from {{host}} (including its state)"

# Ship every app in apps/ to one box. The fleet-of-one-machine case.
selfhost-deploy-all host router="caddy":
    #!/usr/bin/env bash
    set -euo pipefail
    for f in apps/*.toml; do
      just selfhost-deploy "$(basename "$f" .toml)" {{host}} {{router}}
    done

# ---- the wasmCloud lanes: same app, someone else's runtime -------------------
#
# For a cluster somebody else already operates. ADR-0021 took Kubernetes off this
# platform's runtime path deliberately and priced it, so this is interop, not a
# recommendation — `docs/SELFHOST.md` still says start at tier 1.
#
# Two axes, four manifests, one app spec:
#   --topology fused|linked    one wac-composed artifact, or components wired by links
#   --api      v1|v2           core.oam.dev/v1beta1 (wadm 0.21), or wasmcloud.dev/v1alpha1
#
# No `wash`: wash 2.x removed `wash app put`, but the API underneath it is a set of
# NATS subjects both versions still serve, and those are what these recipes use.

wasmcloud_lattice := env_var_or_default("WASMCLOUD_LATTICE", "default")
wasmcloud_registry := env_var_or_default("WASMCLOUD_REGISTRY", "registry.wasmcloud.svc.cluster.local:5000")
# Where THIS machine reaches the registry to push. In-cluster the host pulls from
# the name above; from a laptop that name does not resolve, so pushes go via the
# NodePort. Two names for one registry, and conflating them is why a push succeeds
# and a pull then fails.
wasmcloud_push_registry := env_var_or_default("WASMCLOUD_PUSH_REGISTRY", "localhost:30500")
