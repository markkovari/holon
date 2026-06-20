// Boot the vet-clinic backend + static SPA on PORT (default 3000). Seeds the
// three roles + a demo user per role, then prints the credentials.
//
// Persistence: by default the KV store is in-memory (gone on restart). A variant
// example can make it durable by setting two env vars before launching this:
//   VET_KV_BACKEND=sqlite|redis|nats   (selects the backend kind)
//   VET_KV_BACKEND_MODULE=<abs path>   (a module exporting load/write/remove)
// We hydrate that backend into the shim's mirror BEFORE building the app (the
// seed step writes to KV, so the durable store must be wired up first).

import { initBackend, drainBackend } from "./shims/keyvalue.js";

const PORT = Number(process.env.PORT ?? 3000);

async function main() {
  const backendModule = process.env.VET_KV_BACKEND_MODULE;
  await initBackend(backendModule ? () => import(backendModule) : undefined);

  // Import AFTER the backend is hydrated: buildApp() seeds roles/users, which
  // writes to KV — those writes must reach the durable store.
  const { buildApp } = await import("./app.js");
  const { app, demoUsers } = buildApp({ logger: true, serveStatic: true });

  // Graceful shutdown: flush any in-flight async write-through to the durable
  // backend before exiting, so nothing is lost on a normal stop (Ctrl-C / SIGTERM).
  for (const sig of ["SIGINT", "SIGTERM"] as const) {
    process.on(sig, () => {
      void (async () => {
        try {
          await app.close();
          await drainBackend();
        } finally {
          process.exit(0);
        }
      })();
    });
  }

  await app.listen({ port: PORT, host: "0.0.0.0" });
  const backend = process.env.VET_KV_BACKEND ?? "memory";
  console.log(`\n🐾 vet-clinic up on http://localhost:${PORT}  (kv backend: ${backend})\n`);
  console.log("Demo logins (all under tenant acme-vet):");
  for (const u of demoUsers) {
    console.log(`  ${u.role.padEnd(10)}  ${u.email}  /  ${u.password}`);
  }
  console.log("\nEvery capability is an unmodified comp component running in-process via jco.\n");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
