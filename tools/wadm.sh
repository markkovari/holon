#!/usr/bin/env bash
# Talk to wadm over NATS, from inside the cluster.
#
# Not `wash`: wash 2.x removed `wash app put`, which is what this repo's older
# wasmCloud manifests still instruct. The API underneath that command is a set of
# NATS subjects (`wadm.api.<lattice>.model.<verb>`) that both wadm versions still
# serve, so this needs no CLI whose command set moves under it.
#
# The payload is COPIED INTO THE POD and read there. Three things that do not work,
# each tried against a real cluster first:
#   * the JSON as an argument here — the shell re-parses it and dies on the first
#     `(` in a description;
#   * `just` piping it in — `just` does not forward stdin to a recipe;
#   * `kubectl exec -i` piping it to `nats req` — `nats` reads stdin only when its
#     payload argument is absent, and kubectl's stdin is not a plain pipe.
# Reading the file inside the pod has none of those problems.
#
#   tools/wadm.sh wadm.api.default.model.list
#   tools/wadm.sh wadm.api.default.model.put manifest.json
set -euo pipefail
SUBJECT="${1:?usage: wadm.sh <subject> [payload-file]}"
PAYLOAD="${2:-}"
NS="${WASMCLOUD_NAMESPACE:-wasmcloud}"

POD=$(kubectl get pod -n "$NS" -o name 2>/dev/null | grep nats-box | head -1 | cut -d/ -f2)
if [ -z "$POD" ]; then
  echo "no nats-box pod in namespace $NS — set WASMCLOUD_NAMESPACE" >&2
  exit 1
fi

if [ -n "$PAYLOAD" ]; then
  kubectl cp "$PAYLOAD" "$NS/$POD:/tmp/wadm-payload.json" >/dev/null
  kubectl exec -n "$NS" "$POD" -- sh -c \
    "nats --server nats://nats:4222 req --raw '$SUBJECT' \"\$(cat /tmp/wadm-payload.json)\"" \
    2>/dev/null
else
  kubectl exec -n "$NS" "$POD" -- \
    nats --server nats://nats:4222 req --raw "$SUBJECT" '' 2>/dev/null
fi
