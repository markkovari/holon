#!/usr/bin/env bash
# wadm's NATS API from inside the cluster, since wash 2.x dropped `wash app put`.
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
  # Copied into the pod: as an argument the shell re-parses the JSON, and `nats` ignores kubectl's piped stdin.
  kubectl cp "$PAYLOAD" "$NS/$POD:/tmp/wadm-payload.json" >/dev/null
  kubectl exec -n "$NS" "$POD" -- sh -c \
    "nats --server nats://nats:4222 req --raw '$SUBJECT' \"\$(cat /tmp/wadm-payload.json)\"" \
    2>/dev/null
else
  kubectl exec -n "$NS" "$POD" -- \
    nats --server nats://nats:4222 req --raw "$SUBJECT" '' 2>/dev/null
fi
