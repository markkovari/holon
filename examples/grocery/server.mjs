#!/usr/bin/env node
// Grocery Shop Runner (examples/grocery/server.mjs)
//
// Runs the composed WebAssembly component (components/target/grocery_domain.composed.wasm)
// containing the bundled React SPA and real Rust scanline decoder via comp-host.
//
// ZERO MOCKING: pure compute WebAssembly decoder.

import { spawn } from "node:child_process";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { existsSync } from "node:fs";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, "../..");
const COMPONENT = join(ROOT, "components/target/grocery_domain.composed.wasm");
const HOST_BIN = join(ROOT, "host/target/release/comp-host");
const PORT = process.env.PORT || "3055";

if (!existsSync(COMPONENT)) {
  console.error(`Error: Composed component not found at ${COMPONENT}.`);
  console.error("Run `just compose-grocery` first.");
  process.exit(1);
}

if (!existsSync(HOST_BIN)) {
  console.error(`Error: comp-host binary not found at ${HOST_BIN}.`);
  console.error("Run `cargo build --release --bin comp-host` in host/ first.");
  process.exit(1);
}

console.log(`Starting Holon Grocery App on http://0.0.0.0:${PORT} via comp-host...`);

const child = spawn(
  HOST_BIN,
  [
    "--component",
    COMPONENT,
    "--addr",
    `0.0.0.0:${PORT}`,
    "--app",
    "grocery",
    "--config",
    "default-tenant=grocery",
  ],
  { stdio: "inherit", cwd: ROOT }
);

child.on("exit", (code) => {
  process.exit(code ?? 0);
});
