#!/usr/bin/env bash
# Local, repeatable proof that the wasmCloud 2.x / Kubernetes lane's daemon
# story (docs/SELFHOST.md) actually works — meaning the daemon SERVES A REAL
# REQUEST, not just that the Deployment/Service YAML applies. Composes
# fs-watcher, builds the comp-fswatch daemon image, renders + applies the v2
# manifest (`holon wadm render --api v2`) to a REAL runtime-operator, hits the
# daemon directly with the request it actually understands, then hits the app
# through the operator-maintained Service and expects the same answer it
# would give on any other lane.
#
# Not CI — this needs a live cluster (kind/k3d/OrbStack, whatever the current
# kubectl context points at) with the runtime-operator chart already
# installed, which is real infrastructure a GitHub Actions runner does not
# have for free.
#
# The daemon image is loaded LOCALLY (imagePullPolicy: Never), not pushed to
# the in-cluster registry the wasm component uses. That registry only speaks
# plain HTTP, and the node's container runtime — separate software from the
# wasmCloud host's own OCI puller, with its own trust config — refused to
# pull from it (ErrImagePull: "http: server gave HTTP response to HTTPS
# client"). A `docker build` on a cluster whose node shares the host's image
# store (true of kind, k3d, and OrbStack's built-in k8s) needs no registry at
# all for a locally-built image; a cluster where that is not true needs its
# registry to actually be TLS-terminated (GHCR, ECR, ...), same as any real
# deployment would use. Either way this is a property of the LOCAL DEV
# CLUSTER, so this script patches the image in after applying the rendered
# manifest rather than changing what the renderer itself produces — a real
# cluster's Deployment is correct as rendered.
#
# Prereqs:
#   - kubectl context pointing at a cluster with the runtime-operator
#     installed (a `Host` CR should already show READY=True: run
#     `kubectl get hosts.runtime.wasmcloud.dev -A`) and sharing an image
#     store with the local `docker build` (kind/k3d/OrbStack all do)
#   - an OCI registry reachable in-cluster as a Service, for the wasm
#     component only; this script assumes one named `registry` in NAMESPACE
#     below (see PR #243's session notes for how one was stood up on
#     OrbStack: `kubectl run registry --image=registry:2` + `kubectl expose`)
#   - wkg, docker, kubectl on PATH
set -euo pipefail
cd "$(dirname "$0")/../.."

NAMESPACE="${NAMESPACE:-wasmcloud-system}"
REGISTRY_SVC="registry.${NAMESPACE}.svc.cluster.local:5000"
LOCAL_PORT="${LOCAL_PORT:-5099}"
APP=fs-watcher

fail() { echo "FAIL: $1" >&2; exit 1; }

command -v kubectl >/dev/null || fail "kubectl not on PATH"
command -v wkg >/dev/null || fail "wkg not on PATH (cargo install wkg)"
command -v docker >/dev/null || fail "docker not on PATH"

kubectl get ns "$NAMESPACE" >/dev/null 2>&1 || fail "no '$NAMESPACE' namespace — install the runtime-operator chart first"
ready=$(kubectl get hosts.runtime.wasmcloud.dev -n "$NAMESPACE" \
  -o jsonpath='{.items[0].status.conditions[?(@.type=="Ready")].status}' 2>/dev/null || echo "")
[ "$ready" = "True" ] || fail "no READY=True Host in '$NAMESPACE' — is the runtime-operator actually up?"
kubectl get svc registry -n "$NAMESPACE" >/dev/null 2>&1 || fail "no 'registry' Service in '$NAMESPACE' (see prereqs above)"

echo "==> composing $APP"
cargo xtask compose "$APP"

echo "==> building the daemon image (loaded locally, not pushed — see header)"
docker build -f reconciler/Dockerfile.daemon --build-arg DAEMON=comp-fswatch -t comp-fswatch:local .

echo "==> port-forwarding the in-cluster registry to 127.0.0.1:${LOCAL_PORT}"
kubectl port-forward -n "$NAMESPACE" svc/registry "${LOCAL_PORT}:5000" >/tmp/k8s-verify-pf.log 2>&1 &
PF_PID=$!
trap 'kill $PF_PID 2>/dev/null' EXIT
for _ in $(seq 1 20); do
  curl -fsS "http://127.0.0.1:${LOCAL_PORT}/v2/" >/dev/null 2>&1 && break
  sleep 0.5
done
curl -fsS "http://127.0.0.1:${LOCAL_PORT}/v2/" >/dev/null 2>&1 || fail "registry port-forward never came up"

echo "==> pushing the wasm component (the operator's own OCI puller handles this fine)"
wkg oci push --insecure "127.0.0.1:${LOCAL_PORT}" "127.0.0.1:${LOCAL_PORT}/${APP}:latest" \
  "components/target/${APP}.composed.wasm"

echo "==> rendering the v2 manifest"
cargo build --release --manifest-path cli/Cargo.toml
./cli/target/release/holon wadm render "apps/${APP}.toml" \
  --api v2 --namespace "$NAMESPACE" --registry "$REGISTRY_SVC" --replicas 1 \
  --out /tmp/k8s-verify-manifest.yaml

echo "==> applying it"
kubectl delete workload "$APP" -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1
kubectl apply -f /tmp/k8s-verify-manifest.yaml

echo "==> pointing the daemon Deployment at the local image (see header)"
kubectl patch deployment fswatch-daemon -n "$NAMESPACE" --type=json -p '[
  {"op":"replace","path":"/spec/template/spec/containers/0/image","value":"comp-fswatch:local"},
  {"op":"add","path":"/spec/template/spec/containers/0/imagePullPolicy","value":"Never"}
]'

echo "==> waiting for the Workload to report Ready"
for _ in $(seq 1 30); do
  ready=$(kubectl get workload "$APP" -n "$NAMESPACE" \
    -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null || echo "")
  [ "$ready" = "True" ] && break
  sleep 1
done
[ "$ready" = "True" ] || fail "Workload never reached Ready — kubectl describe workload $APP -n $NAMESPACE"
echo "    Workload Ready=true"

echo "==> waiting for the daemon Deployment"
for _ in $(seq 1 30); do
  daemon_ready=$(kubectl get deploy fswatch-daemon -n "$NAMESPACE" -o jsonpath='{.status.readyReplicas}' 2>/dev/null || echo "0")
  [ "$daemon_ready" = "1" ] && break
  sleep 1
done
[ "$daemon_ready" = "1" ] || fail "fswatch-daemon never reached Ready — kubectl describe pod -l app=fswatch-daemon -n $NAMESPACE"
echo "    fswatch-daemon Ready=true"

echo "==> hitting the daemon DIRECTLY — the request it actually understands, not the app's"
kubectl port-forward -n "$NAMESPACE" svc/fswatch-daemon 18098:8000 >/tmp/k8s-verify-daemon-pf.log 2>&1 &
DAEMON_PF_PID=$!
trap 'kill $PF_PID $DAEMON_PF_PID 2>/dev/null' EXIT
sleep 2
code=$(curl -s -o /tmp/k8s-verify-daemon-response.json -w '%{http_code}' \
  -X POST -H 'content-type: application/json' -d '{"dir":"/var/log"}' \
  "http://127.0.0.1:18098/poll")
body=$(cat /tmp/k8s-verify-daemon-response.json)
[ "$code" = "200" ] || fail "POST /poll direct to the daemon -> $code, expected 200 (body: $body)"
echo "$body" | grep -q '"error"' && fail "POST /poll -> 200 but the body says an error: $body"
echo "    POST /poll -> 200: $body"
kill $DAEMON_PF_PID 2>/dev/null

echo "==> now the app, through the operator-maintained Service — the full path"
kubectl port-forward -n "$NAMESPACE" svc/hostgroup-default 18099:80 >/tmp/k8s-verify-app-pf.log 2>&1 &
APP_PF_PID=$!
trap 'kill $PF_PID $APP_PF_PID 2>/dev/null' EXIT
sleep 2
code=$(curl -s -o /tmp/k8s-verify-response.json -w '%{http_code}' \
  -H "Host: ${APP}.example.com" "http://127.0.0.1:18099/api/watch")
[ "$code" = "200" ] || fail "GET /api/watch -> $code, expected 200 (body: $(cat /tmp/k8s-verify-response.json))"
echo "    GET /api/watch -> 200: $(cat /tmp/k8s-verify-response.json)"

echo "==> cleaning up"
kubectl delete workload "$APP" -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1
kubectl delete deployment fswatch-daemon -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1
kubectl delete service fswatch-daemon -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1

echo "PASS (full end-to-end: daemon answers directly, and the app answers through it)"
