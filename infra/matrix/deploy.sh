#!/usr/bin/env bash
# Deploys the private Matrix homeserver (Synapse).
#
#   ./deploy.sh local                 # this machine, server_name `localhost` (to try it)
#   ./deploy.sh Malna                 # an ssh host; server_name = its tailnet DNS name
#   SERVER_NAME=x.ts.net ./deploy.sh Malna
#   APPSERVICE_REG=~/.holon-matrix/holon-agents.yaml ./deploy.sh Malna   # also load the agent bridge
#
# Idempotent: re-running keeps the secrets, the signing key and all data, and
# only re-renders the config and restarts. Nothing here exposes the server: it
# listens on loopback. `tailscale.sh` is what makes it reachable (privately).
set -euo pipefail

TARGET=${1:?usage: deploy.sh local|<ssh host>}
HERE=$(cd "$(dirname "$0")" && pwd)
REMOTE_DIR=${REMOTE_DIR:-holon-matrix}
SYNAPSE_TAG=${SYNAPSE_TAG:-latest}
SYNAPSE_PORT=${SYNAPSE_PORT:-8008}

# Run a command on the target (or here, for `local`).
# Seamless access: if ~/.ssh/holon_<host> exists (a dedicated deploy key; see README),
# use it, unless SSH_OPTS says otherwise.
if [ -z "${SSH_OPTS:-}" ]; then
  _k="$HOME/.ssh/holon_$(printf %s "${TARGET:-}" | tr 'A-Z' 'a-z')"
  [ -f "$_k" ] && SSH_OPTS="-i $_k -o IdentitiesOnly=yes"
fi
# Commands are piped to `bash -s`: the remote login shell may not be bash (malna's is
# fish). SSH_OPTS is for things like a shared ControlPath; none are required.
on() { if [ "$TARGET" = local ]; then bash -c "$1"; else printf '%s\n' "$1" | ssh ${SSH_OPTS:-} -o BatchMode=yes "$TARGET" bash -s; fi; }

if [ "$TARGET" = local ]; then
  DIR=${LOCAL_DIR:-$HERE/local}
  SERVER_NAME=${SERVER_NAME:-localhost}
else
  DIR=$REMOTE_DIR
  # The tailnet DNS name is the natural, permanent server name.
  SERVER_NAME=${SERVER_NAME:-$(on "tailscale status --json | python3 -c 'import json,sys; print(json.load(sys.stdin)[\"Self\"][\"DNSName\"].rstrip(\".\"))'")}
fi
echo "target=$TARGET  dir=$DIR  server_name=$SERVER_NAME"

# docker or podman, whichever the target has.
RT=$(on 'command -v docker >/dev/null && echo docker || { command -v podman >/dev/null && echo podman; }')
[ -n "$RT" ] || { echo "neither docker nor podman on $TARGET" >&2; exit 1; }
COMPOSE=$(on "if $RT compose version >/dev/null 2>&1; then echo '$RT compose'; elif command -v ${RT}-compose >/dev/null; then echo ${RT}-compose; fi")
[ -n "$COMPOSE" ] || { echo "no '$RT compose' on $TARGET" >&2; exit 1; }
echo "runtime=$RT compose='$COMPOSE'"

on "mkdir -p '$DIR/data'"
if [ "$TARGET" = local ]; then
  cp "$HERE/compose.yaml" "$HERE/homeserver.yaml.tmpl" "$DIR/"
else
  scp -q ${SSH_OPTS:-} "$HERE/compose.yaml" "$HERE/homeserver.yaml.tmpl" "$TARGET:$DIR/"
fi

# An application service registration (the agent bridge), if given.
if [ -n "${APPSERVICE_REG:-}" ]; then
  on "mkdir -p '$DIR/appservice'"
  if [ "$TARGET" = local ]; then cp "$APPSERVICE_REG" "$DIR/appservice/holon-agents.yaml"; else scp -q ${SSH_OPTS:-} "$APPSERVICE_REG" "$TARGET:$DIR/appservice/holon-agents.yaml"; fi
  on "chmod 600 '$DIR/appservice/holon-agents.yaml'"
fi

# Secrets are generated once, on the target, and never leave it.
on "cd '$DIR' && umask 077 && [ -f .env ] || { printf 'REGISTRATION_SECRET=%s\nMACAROON_SECRET=%s\nFORM_SECRET=%s\n' \$(openssl rand -hex 32) \$(openssl rand -hex 32) \$(openssl rand -hex 32) > .env; }"

# Signing key and log config come from Synapse's own generator, once.
on "cd '$DIR' && [ -f 'data/$SERVER_NAME.signing.key' ] || $RT run --rm -v \"\$PWD/data:/data\" -e SYNAPSE_SERVER_NAME='$SERVER_NAME' -e SYNAPSE_REPORT_STATS=no matrixdotorg/synapse:$SYNAPSE_TAG generate"

# Render the config from the template, then install it through a throwaway
# container: the generator above leaves data/ owned by Synapse's own user
# (uid 991), which the host user cannot write — and making it world-writable
# or reaching for sudo would be the wrong fix.
on "cd '$DIR' && . ./.env && sed -e 's|@@SERVER_NAME@@|$SERVER_NAME|g' -e \"s|@@REGISTRATION_SECRET@@|\$REGISTRATION_SECRET|\" -e \"s|@@MACAROON_SECRET@@|\$MACAROON_SECRET|\" -e \"s|@@FORM_SECRET@@|\$FORM_SECRET|\" homeserver.yaml.tmpl > homeserver.rendered.yaml && { [ -f appservice/holon-agents.yaml ] && printf 'app_service_config_files:\\n  - /data/appservices/holon-agents.yaml\\n' >> homeserver.rendered.yaml || true; } && chmod 600 homeserver.rendered.yaml"
on "cd '$DIR' && $RT run --rm --entrypoint sh -v \"\$PWD:/src:ro\" -v \"\$PWD/data:/data\" matrixdotorg/synapse:$SYNAPSE_TAG -c 'cp /src/homeserver.rendered.yaml /data/homeserver.yaml && chown 991:991 /data/homeserver.yaml && chmod 600 /data/homeserver.yaml && if [ -d /src/appservice ]; then mkdir -p /data/appservices && cp /src/appservice/*.yaml /data/appservices/ && chown -R 991:991 /data/appservices && chmod 600 /data/appservices/*.yaml; fi' && rm -f homeserver.rendered.yaml"

# --force-recreate: the config is a bind mount, so compose alone would not notice that
# it changed (an added appservice, say) and would leave the old Synapse running.
# Re-running therefore restarts Synapse for a few seconds.
on "cd '$DIR' && SYNAPSE_TAG='$SYNAPSE_TAG' SYNAPSE_PORT='$SYNAPSE_PORT' $COMPOSE up -d --force-recreate"
echo "waiting for Synapse..."
for _ in $(seq 1 40); do
  on "curl -fsS http://127.0.0.1:$SYNAPSE_PORT/_matrix/client/versions >/dev/null 2>&1" && { echo "up: http://127.0.0.1:$SYNAPSE_PORT on $TARGET"; exit 0; }
  sleep 3
done
echo "Synapse did not come up; logs:" >&2
on "cd '$DIR' && $COMPOSE logs --tail 40"
exit 1
