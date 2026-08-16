import { defineConfig } from "@playwright/test";

// The console's browser suite. `globalSetup` brings up the real stack — SurrealDB,
// a seeded run, and `comp-host` serving the composed component — because the
// point of an e2e here is that nothing below the browser is stubbed.
//
// One worker: the suite shares one seeded database, and a second worker would
// race the first one's reads against its own seed. Two tests do not need the
// parallelism, and a flaky e2e is worse than a slow one.
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  workers: 1,
  timeout: 30_000,
  globalSetup: "./e2e/setup.ts",
  use: {
    baseURL: process.env.CONSOLE_URL ?? "http://127.0.0.1:3056",
    // A trace on the first retry only: enough to debug a failure, not enough to
    // slow the happy path or fill a disk.
    trace: "on-first-retry",
  },
});
