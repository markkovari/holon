# studio — the composition studio (e2e + xyflow SPA)

See **[docs/apps/STUDIO.md](../../STUDIO.md)** for what this is and why.

```
tests/studio.rs   the e2e — reflect, refuse, plan, emit, compose, then RUN the
                  composed component and check the emitted script + .wac against
                  the real `wac` CLI
ui/               React + @xyflow/react SPA (Vite + Tailwind) -> dist/
```

```bash
just host-studio          # :3054, palette seeded with all 109 components
just seed-studio          # (re)feed the palette into an already-running studio
just e2e-studio           # the full ladder
cd ../../components && cargo test -p wit-reflect
```

The API is usable without the UI:

```bash
# reflect a component
curl -X POST --data-binary @../../components/target/wasm32-wasip2/release/mesh_domain.wasm \
  -H 'content-type: application/wasm' 'localhost:3054/api/components?id=mesh-domain'

# would these two fit? (wac's own subtype check)
curl -s localhost:3054/api/satisfies -d '{"socket":"mesh-domain","plug":"zip"}' \
  -H 'content-type: application/json'      # -> {"interfaces":[]}

# the same graph as a wasmCloud workload
curl -s localhost:3054/api/emit -H 'content-type: application/json' -d '{
  "nodes":["mesh-domain","record-store"],
  "edges":[{"plug":"record-store","socket":"mesh-domain","iface":"records:store/store@0.1.0"}],
  "form":"workload","meta":{"name":"mesh"}}'

# ...or as a real composed component
curl -s localhost:3054/api/compose -H 'content-type: application/json' \
  -d '{"nodes":["mesh-domain","record-store"],"edges":[...],"root":"mesh-domain"}' \
  -o mesh.composed.wasm
```
