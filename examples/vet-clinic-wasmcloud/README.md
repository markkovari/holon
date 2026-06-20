# vet-clinic-wasmcloud — the auth/RBAC backend on wasmCloud 2.x (Kubernetes)

The production path the jco examples mirror: the **same** `auth-guard` (composed
with rate-limiter + audit-log) and `accounts-app` components, running as real
wasm on a **wasmCloud host in Kubernetes** (the `wasmcloud-operator`), fronted by
the **http-server** provider and persisted to **NATS JetStream KV** via the
**keyvalue-nats** provider. No Node, no jco shim — the `wasi:keyvalue` import is
satisfied by a real provider, chosen here in the deployment manifest, never in WIT.

This was **deployed and verified live** on the in-cluster operator: register →
201, login → 200 (real `sess_`/`ref_` tokens), and the account reads back across
logins from the `vetclinic` NATS KV bucket.

## Scope

The vet **domain** (pets/appointments/notes) in the jco examples is JS glue around
the components — there's no wasm host for that on wasmCloud. This deploys the part
that IS pure wasm: the register/login/3-role-RBAC HTTP surface (`accounts-app`)
over `auth-guard`, durably persisted. That's the real, component-native auth
backend a vet-clinic frontend calls.

## Files

- `k8s/host.yaml` — `WasmCloudHostConfig` (operator CRD): one host in the
  `vet-clinic` lattice, NATS as the JetStream backend, allows the in-cluster
  registry. **Host version `1.6.0`** (see gotcha below).
- `k8s/app.yaml` — OAM `Application`: `auth-guard` + `accounts-app` + the three
  providers (http-server :8081, keyvalue-nats bucket `vetclinic`, http-client),
  with every link wired. Images pulled `oci://` from the in-cluster registry.
- `wadm.yaml` — the equivalent wasmCloud **1.x** (`wash app deploy`) manifest, for
  a non-Kubernetes host. (The k8s path above is the one that was run live.)

## Deploy (Kubernetes + operator)

Prereqs: a cluster with the `wasmcloud-operator`, `wadm`, and JetStream NATS
running (the repo's `infra/k8s` sets these up), plus the in-cluster registry
reachable from the host at `registry.wasmcloud.svc.cluster.local:5000` (NodePort
`:30500` from the host machine). Components built: `just build && just compose`.

```bash
# 1. push the two components to the in-cluster registry (host-side via :30500)
wash oci push --insecure localhost:30500/vet-auth-guard:0.1.0   ../../components/target/auth_guard.composed.wasm
wash oci push --insecure localhost:30500/vet-accounts-app:0.1.0 ../../components/target/wasm32-wasip1/release/accounts_app.wasm

# 2. host + app
kubectl create namespace vet-clinic
kubectl apply -f k8s/host.yaml      # wait until the host pod is 2/2 Running
kubectl apply -f k8s/app.yaml       # wadm schedules components + providers

# 3. exercise it
kubectl port-forward -n vet-clinic deploy/vet-clinic-host 8088:8081 &
curl -X POST localhost:8088/register -H 'content-type: application/json' \
  -d '{"email":"owner@acme-vet.test","password":"ownerpass1","tenant":"acme-vet"}'
curl -X POST localhost:8088/login -H 'content-type: application/json' \
  -d '{"email":"owner@acme-vet.test","password":"ownerpass1","tenant":"acme-vet"}'
# the account persists in the `vetclinic` NATS KV bucket — log in again after a
# host restart and it's still there.
```

Check status: `kubectl get application -n vet-clinic` (→ `Deployed`) and the host
log `kubectl logs -n vet-clinic deploy/vet-clinic-host -c wasmcloud-host`.

## Gotcha — wasi:http version skew (why host version 1.6.0)

On host **`1.4.1`** the components failed to scale with:

```
component imports instance `wasi:http/types@0.2.0`, but a matching implementation
was not found in the linker … instance export `[method]incoming-body.stream` has
the wrong type … resource type mismatch
```

The components were built against a `wasi:http@0.2.0` snapshot whose `incoming-body`
resource shape differs from the wasmtime bundled in wasmCloud 1.4.1. Bumping the
host to **`1.6.0`** (newer wasmtime) resolved it — both components then started and
the HTTP + KV path worked. Pin the host version to one whose wasmtime matches the
`wasi:http` your components target.

## Teardown

```bash
kubectl delete -f k8s/app.yaml
kubectl delete -f k8s/host.yaml
kubectl delete namespace vet-clinic
```
