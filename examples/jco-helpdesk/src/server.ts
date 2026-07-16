// Serve the composed helpdesk-domain HTTP component in-process via jco's WASI
// HTTPServer. The component exports `wasi:http/incoming-handler` — the SAME
// shape a wasmCloud http-server provider drives — so this Node harness and a
// wasmCloud host run the identical bytes. The non-standard imports (keyvalue,
// config) are the local shims; everything else is jco's default WASI.

import { HTTPServer } from "@bytecodealliance/preview2-shim/http";
// the transpiled component module (exports `incomingHandler`).
import * as component from "../gen/helpdesk_domain.composed.js";

const PORT = Number(process.env.PORT ?? 3007);
const HOST = process.env.HOST ?? "0.0.0.0";

const server = new HTTPServer(component.incomingHandler);
server.listen(PORT, HOST);
console.log(`helpdesk-domain (composed wasm) serving on http://${HOST}:${PORT}`);
