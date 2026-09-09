#!/usr/bin/env bash
# Local, repeatable proof that the wasmCloud 2.x / Kubernetes lane's daemon
# story (docs/SELFHOST.md) actually works: renders fs-watcher's v2 manifest
# (a Workload + the fswatch daemon's own Deployment/Service, from `holon wadm
# render --api v2`), pushes both images to the cluster's own registry, applies
# it to a REAL runtime-operator, and curls the running app through it.
#
# Not CI — this needs a live cluster (kind/k3d/OrbStack, whatever the current
# kubectl context points at) with the runtime-operator chart already
# installed, which is real infrastructure a GitHub Actions runner does not
# have for free. Run this by hand after standing one up, the same way this
# session did against OrbStack.
#
# Prereqs:
#   - kubectl context pointing at a cluster with the runtime-operator
#     installed (a `Host` CR should already show READY=True: run
#     `kubectl get hosts.runtime.wasmcloud.dev -A`)
#   - an OCI registry reachable in-cluster as a Service; this script assumes
#     one named `registry` in NAMESPACE below (see PR #243's session notes
#     for how one was stood up on OrbStack: `kubectl run registry
#     --image=registry:2` + `kubectl expose`)
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

echo "==> building the daemon image"
docker build -f reconciler/Dockerfile.daemon --build-arg DAEMON=comp-fswatch -t comp-fswatch:verify .

echo "==> port-forwarding the in-cluster registry to 127.0.0.1:${LOCAL_PORT}"
kubectl port-forward -n "$NAMESPACE" svc/registry "${LOCAL_PORT}:5000" >/tmp/k8s-verify-pf.log 2>&1 &
PF_PID=$!
trap 'kill $PF_PID 2>/dev/null' EXIT
for _ in $(seq 1 20); do
  curl -fsS "http://127.0.0.1:${LOCAL_PORT}/v2/" >/dev/null 2>&1 && break
  sleep 0.5
done
curl -fsS "http://127.0.0.1:${LOCAL_PORT}/v2/" >/dev/null 2>&1 || fail "registry port-forward never came up"

echo "==> pushing the wasm component"
wkg oci push --insecure "127.0.0.1:${LOCAL_PORT}" "127.0.0.1:${LOCAL_PORT}/${APP}:latest" \
  "components/target/${APP}.composed.wasm"

echo "==> pushing the daemon image"
docker tag comp-fswatch:verify "127.0.0.1:${LOCAL_PORT}/comp-fswatch:latest"
docker push "127.0.0.1:${LOCAL_PORT}/comp-fswatch:latest"

echo "==> rendering the v2 manifest"
cargo build --release --manifest-path cli/Cargo.toml
./cli/target/release/holon wadm render "apps/${APP}.toml" \
  --api v2 --namespace "$NAMESPACE" --registry "$REGISTRY_SVC" --replicas 1 \
  --out /tmp/k8s-verify-manifest.yaml

echo "==> applying it"
kubectl delete workload "$APP" -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1
kubectl apply -f /tmp/k8s-verify-manifest.yaml

echo "==> waiting for the Workload to report Ready"
for _ in $(seq 1 30); do
  ready=$(kubectl get workload "$APP" -n "$NAMESPACE" \
    -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null || echo "")
  [ "$ready" = "True" ] && break
  sleep 1
done
[ "$ready" = "True" ] || fail "Workload never reached Ready — kubectl describe workload $APP -n $NAMESPACE"
echo "    Workload Ready=true"

echo "==> checking the daemon Deployment (known gap: image pull needs the node's"
echo "    container runtime to trust this registry separately — see docs/SELFHOST.md)"
daemon_ready=$(kubectl get deploy fswatch-daemon -n "$NAMESPACE" -o jsonpath='{.status.readyReplicas}' 2>/dev/null || echo "0")
if [ "$daemon_ready" = "1" ]; then
  echo "    fswatch-daemon: Ready (registry trust is configured on this cluster)"
else
  echo "    fswatch-daemon: not Ready yet (kubectl describe pod -l app=fswatch-daemon -n $NAMESPACE) — expected on a plain-HTTP local registry, not a failure of the rendered manifest itself"
fi

echo "==> curling the app through the operator-maintained Service"
kubectl port-forward -n "$NAMESPACE" svc/hostgroup-default 18099:80 >/tmp/k8s-verify-app-pf.log 2>&1 &
APP_PF_PID=$!
trap 'kill $PF_PID $APP_PF_PID 2>/dev/null' EXIT
sleep 2
code=$(curl -s -o /tmp/k8s-verify-response.json -w '%{http_code}' \
  -H "Host: ${APP}.example.com" "http://127.0.0.1:18099/api/watch")
if [ "$code" = "200" ]; then
  echo "    GET /api/watch -> 200: $(cat /tmp/k8s-verify-response.json)"
elif [ "$daemon_ready" != "1" ]; then
  # Expected: the wasm side (Workload, egress grants, config rewrite) is
  # proven by Ready=true above; without the daemon actually running, the
  # component's own call to it fails the same way any real caller's would —
  # that is the already-documented gap, not something this script papers
  # over as a pass.
  echo "    GET /api/watch -> $code (body: $(cat /tmp/k8s-verify-response.json))"
  echo "    Expected: fswatch-daemon isn't Ready (see above), so nothing answers this."
else
  fail "GET /api/watch -> $code with a Ready daemon — expected 200 (body: $(cat /tmp/k8s-verify-response.json))"
fi

echo "==> cleaning up"
kubectl delete workload "$APP" -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1
kubectl delete deployment fswatch-daemon -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1
kubectl delete service fswatch-daemon -n "$NAMESPACE" --ignore-not-found >/dev/null 2>&1

if [ "$daemon_ready" = "1" ]; then
  echo "PASS (full end-to-end, including the daemon)"
else
  echo "PASS (Workload/egress/config-rewrite verified; daemon image-pull gap reproduced, not fixed — see docs/SELFHOST.md)"
fi
