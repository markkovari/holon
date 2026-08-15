# mesh — resilience playground (e2e + flaky upstream + SPA)

See **[docs/apps/MESH.md](../../MESH.md)** for what this is and why, and
`tools/screencast/mesh.mjs` for the recorder behind its gif.

```
src/bin/flaky.rs   the deliberately unreliable upstream (std only, no deps)
tests/mesh.rs      the e2e: retry, breaker trip/shed/recover, SLO, unreachable
ui/                the React + shadcn SPA (Vite + Tailwind) -> dist/
```

```bash
just host-mesh     # SPA + host on :3050, flaky upstream on :3051
just e2e-mesh      # the full ladder against the real upstream
just mesh-upstream # the upstream alone (survives host restarts)
```

The upstream misbehaves on demand, per request — that is how the tests stay
deterministic:

```bash
curl 'localhost:3051/hit'                  # 200
curl 'localhost:3051/hit?fail=1'           # 500
curl 'localhost:3051/hit?fail_n=2&id=x'    # 500, 500, then 200 (a blip)
curl 'localhost:3051/hit?delay=400'        # 200, late
curl 'localhost:3051/count?id=x'           # how many times it was really called
```

That last one is the point: while the circuit is open, it stops going up.
