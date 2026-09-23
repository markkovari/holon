# studio — the composition studio (e2e + xyflow SPA)

See **[docs/apps/STUDIO.md](../../docs/apps/STUDIO.md)** for what this is and why.

```
tests/studio.rs   the e2e — reflect, refuse, plan, emit, compose, then RUN the
                  composed component and check the emitted script + .wac against
                  the real `wac` CLI
ui/               React + @xyflow/react SPA (Vite + Tailwind) -> dist/
```

```bash
cargo xtask host studio   # :3054 — the palette starts empty; feed it with the loop below
# (re)feed the palette every component in the repo, into an already-running studio:
for f in components/target/wasm32-wasip2/release/*.wasm; do
  curl -s -o /dev/null -X POST --data-binary "@$f" -H 'content-type: application/wasm' \
    "localhost:3054/api/components?id=$(basename "$f" .wasm | tr _ -)"
done
cargo xtask e2e studio    # the full ladder
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
