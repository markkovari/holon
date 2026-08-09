# 0039 — comp vs wasmCloud 2.x, same component, both machines

Status: accepted. Supersedes every informal "how much faster are we" claim, all of
which compared different components on different machines through different paths.

## The setup

One component — `gate-domain`, the same `.wasm` file — deployed to both platforms on
this Mac, loaded by the same generator with the same body.

- **wasmCloud**: chart `runtime-operator` 2.5.2 in OrbStack k8s (operator, gateway,
  NATS, one `wash` 2.5.2 host), component pushed to a local OCI registry, deployed as
  a `WorkloadDeployment` with `poolSize: 8`, `maxInvocations: 200`, and
  `hostInterfaces` for `wasi:http/incoming-handler` plus `wasi:keyvalue`
  store/atomics/batch.
- **comp**: one `comp-host`, one `comp-ingress`, one replica, `--kv nats`.

The body sets `capacity`/`refill` high so both exercise the **allow** path; at the
default bucket a token limiter answers 429 from a cheap branch that touches no
storage (ADR-0036). All four lanes returned 100% `200`.

## The numbers, 20s × 50 connections

| lane | rps | p50 | p99 |
|---|---|---|---|
| wasmcloud, via gateway | 1 567 | 12.0 ms | 229 ms |
| wasmcloud, direct to host | 1 708 | 10.9 ms | 227 ms |
| comp, via ingress | 6 246 | 7.7 ms | 15.1 ms |
| comp, direct to host | 6 199 | 7.8 ms | 15.2 ms |

**Host to host, comp serves 3.6× the throughput** (6 199 vs 1 708). Both proxies are
nearly free on their own platform: comp's ingress costs under 1%, wasmCloud's gateway
about 8%.

Memory, while serving: the `wash` host pod held **154 Mi**, `comp-host` **54 Mi**.
Control planes are not compared — wasmCloud's adds gateway (38 Mi) and operator
(63 Mi), comp's adds a reconciler and a control-plane component, and nothing measured
them symmetrically.

## The control that saves the throughput claim and kills the latency one

wasmCloud is reached over OrbStack's cluster network; comp over loopback. That
asymmetry could be the entire result, so it was measured rather than argued: a
trivial nginx in the same namespace, reached the same way by the same generator.

```
nginx via ClusterIP:  9 333 rps   p50 0.1 ms   p99 206 ms
```

Two conclusions, and they point in opposite directions:

1. **The throughput gap is real.** The cluster path sustains 9 333 rps, five times
   what wasmCloud delivered through it. wasmCloud's 1 708 is bound by wasmCloud, not
   by OrbStack.
2. **The tail comparison is not.** nginx — which does nothing — shows p99 206 ms on
   that path, against wasmCloud's 227 ms. The tail is the network, not the runtime.
   **No p99 claim can be made from this run**, and the 15 ms vs 227 ms difference in
   the table must not be read as a platform result.

Without the control, the honest-looking sentence "comp is 3.6× faster with a 15×
better tail" would have been half wrong — and it is exactly the half that would have
been quoted.

## The second run, on malna, which has no cluster in the way

The Mac comparison has one uncomfortable asymmetry: wasmCloud is reached over
OrbStack's cluster network, comp over loopback. malna removes it entirely — no k8s,
no VM. wasmCloud runs the **same 2.5.2 image** under podman (so the host matches the
control plane exactly), comp runs its aarch64 binary, both listen on 0.0.0.0, and
both are loaded from this Mac over the same LAN.

| Pi, 20s × 30 connections | rps | p50 | p99 | RSS |
|---|---|---|---|---|
| wasmcloud (wash 2.5.2, podman) | 240 | 71.4 ms | 608 ms | 82 MiB (+21 MiB second process) |
| comp (comp-host) | **549** | **41.2 ms** | **369 ms** | **51 MiB** |

**2.3× the throughput, and here the latency numbers are comparable** — identical
network path on both sides, which is exactly what the Mac run could not claim. So
the honest summary is 3.6× on the Mac, 2.3× on the Pi, and a real but smaller
latency advantage that only this run establishes.

## What wasmCloud does better, measured in passing

Scaled to four replicas across the two hosts, its scheduler placed **3 on the Mac
and 1 on the Pi** — very close to the 10:4 core ratio. comp's placement spreads by
instance count only (ADR-0034) and would have split them 2/2, putting the same load
on a machine with 40% of the cores. Capacity-weighted placement is a thing to steal.

## What this does not say

- **One component, one machine, one shape of request.** `gate` is a small
  compute-plus-keyvalue path. Nothing here generalises to large payloads, streaming,
  or graphs of linked components.
- **Not tuned.** `poolSize: 8` and `maxInvocations: 200` were copied from an existing
  workload; wasmCloud may go faster configured by someone who knows it well. comp's
  side is equally untuned.
- **Different storage wiring.** Both reach NATS KV, but through different code:
  comp's host implements `wasi:keyvalue` directly, wasmCloud's satisfies it through
  the host's own interface plumbing. Some of the gap may be storage, not runtime, and
  this run cannot separate them.
- **Two machines, but no cross-machine load.** Both runs drive one host at a time.
  Nothing here measures wasmCloud's lattice under load the way ADR-0036 drove comp's,
  and no machine was killed mid-flight the way ADR-0035 did.
- **The Pi numbers are small.** 240 vs 549 rps on four cores; conclusions drawn from
  a slow box do not automatically hold on a fast one, which is why both are reported
  rather than averaged.

## Getting wasmCloud onto malna at all

Recorded because none of it is in any documentation and all of it was a dead end
until it wasn't:

- `wash` 2.5.2 is glibc 2.38+; malna is Debian 12 on 2.36. No older chart is
  published on ghcr, and `wasmCloud/wash` has no 2.5.2 source tag (releases stop at
  2.0.0-rc.7), so there is nothing tagged to cross-compile.
- The way through is the **container image**, which is also the only way to guarantee
  the host matches the control plane. `brew services start podman` fails with a bad
  unit file — and is not needed: podman is daemonless, that unit only provides the
  Docker-compat socket.
- Brew's podman then fails with `could not find a working conmon binary`. Everything
  it needs (`conmon`, `crun`, `pasta`) is installed, just in brew's prefix, which
  podman never searches. A `containers.conf` naming `conmon_path` and the `crun`
  runtime fixes it, plus a `policy.json` (`insecureAcceptAnything`) it also wants.

## Worth recording about the setup itself

A `wash` host at 2.3.0 against a 2.5.2 control plane placed workloads and then never
ran them: `Config`, `HostSelection` and `Placement` all True, `Sync` stuck at
`WORKLOAD_STATE_NOT_FOUND`. Nothing in the operator or host logs named a version
mismatch. That is the failure mode ADR-0036 flagged when noting our own subjects
carry no version — wasmCloud versions its control subjects and still produced a
silent skew failure, which argues the versioning is necessary but not sufficient.
