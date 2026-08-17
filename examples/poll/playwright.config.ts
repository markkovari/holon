import { defineConfig } from "@playwright/test";

// The poll's browser suite. `globalSetup` starts `comp-host` on the composed
// component, because the thing being asserted is a COOKIE rule and a rendered SVG —
// neither of which an API test can see. Nothing below the browser is stubbed.
//
// One worker. The suite shares one keyvalue store, and the vote counts asserted in
// one test are the store a parallel test would be writing to. Two tests do not need
// the parallelism and a flaky e2e is worse than a slow one.
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  workers: 1,
  timeout: 30_000,
  globalSetup: "./e2e/setup.ts",
  use: {
    baseURL: process.env.POLL_URL ?? "http://127.0.0.1:3057",
    trace: "on-first-retry",
  },
});
