// Serve the composed vet-domain HTTP component in-process via jco's WASI
// HTTPServer. The component exports `wasi:http/incoming-handler` — the SAME
// shape a wasmCloud http-server provider drives — so this Node harness and a
// wasmCloud host run the identical bytes. The non-standard imports (keyvalue,
// config) are the local shims; everything else is jco's default WASI.

import { HTTPServer } from "@bytecodealliance/preview2-shim/http";
// the transpiled component module (exports `incomingHandler`).
import * as component from "../gen/vet_domain.composed.js";

const PORT = Number(process.env.PORT ?? 3005);

// HTTPServer wants an object with a `handle` — the component's incomingHandler.
const server = new HTTPServer(component.incomingHandler);
server.listen(PORT);
console.log(`vet-domain (composed wasm) serving on http://localhost:${PORT}`);
