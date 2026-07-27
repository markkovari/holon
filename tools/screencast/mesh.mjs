// Screencast: the mesh resilience playground (React + shadcn UI) on the REAL
// running app, in front of the REAL flaky upstream. The story, in order:
//
//   1. a healthy call succeeds on one attempt
//   2. a 300ms response with an SLO of 100ms is a FAILURE despite its 200
//   3. hammering the failing upstream trips the breaker -> OPEN (red)
//   4. further calls come back "shed — circuit open" and the `shed` counter
//      climbs while `calls` does NOT: the upstream is no longer being dialled
//   5. the cooldown runs out and one probe closes the circuit again
//
// Prereq: from repo root  `just host-mesh &`  (SPA on :3050, upstream on :3051)
import { chromium } from "playwright";

const BASE = process.env.MESH_URL || "http://127.0.0.1:3050";
const OUT = new URL("./videos/mesh/", import.meta.url).pathname;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: 1000, height: 760 },
  recordVideo: { dir: OUT, size: { width: 1000, height: 760 } },
  deviceScaleFactor: 1,
});
const page = await ctx.newPage();
await page.goto(BASE);

const set = async (label, value) => {
  const f = page.getByLabel(label);
  await f.fill(String(value));
  await f.blur();
};

try {
  await sleep(900);
  // Shorten the cooldown so the recovery fits in a gif, and turn the SLO on.
  await set("open for ms", 2500);
  await set("slo ms (0 = off)", 100);
  await sleep(500);

  // 1. healthy
  await page.getByRole("button", { name: "Healthy" }).click();
  await sleep(1400);

  // 2. slow == failed (200, but past the SLO)
  await page.getByRole("button", { name: "Slow (300ms)" }).click();
  await sleep(1900);

  // 3. trip it
  await page.getByRole("button", { name: /Hammer it/ }).click();
  await sleep(3200);

  // 4. shed: the calls stop reaching the upstream
  for (let i = 0; i < 2; i++) {
    await page.getByRole("button", { name: "500s" }).click();
    await sleep(1100);
  }
  await sleep(900);

  // 5. cooldown over -> one probe closes it
  await sleep(1800);
  await page.getByRole("button", { name: "Healthy" }).click();
  await sleep(2400);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
