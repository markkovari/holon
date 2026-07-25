// Screencast: the gate traffic-shaping gateway (React + shadcn UI) on the REAL
// running app. Three panels: burst the rate limiter (token bucket -> 200s then
// 429s), burst the throttle (GCRA spacing), and submit items to a batch that
// coalesces and flushes. Recorded at a desktop viewport (three columns).
//
// Prereq: from repo root  `just host-gate &`   (builds the UI, serves on :3044)
import { chromium } from "playwright";

const BASE = process.env.GATE_URL || "http://127.0.0.1:3044";
const OUT = new URL("./videos/gate/", import.meta.url).pathname;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: 1000, height: 640 },
  recordVideo: { dir: OUT, size: { width: 1000, height: 640 } },
  deviceScaleFactor: 1,
});
const page = await ctx.newPage();
await page.goto(BASE);

try {
  await sleep(900);
  // rate limit: a burst drains the bucket to 429.
  await page.getByRole("button", { name: "Burst ×10" }).first().click();
  await sleep(2200);
  // throttle: a burst shows GCRA spacing.
  await page.getByRole("button", { name: "Burst ×10" }).nth(1).click();
  await sleep(2200);
  // batch: submit samples until it flushes (max size 4).
  for (let i = 0; i < 4; i++) {
    await page.getByRole("button", { name: "Submit a sample" }).click();
    await sleep(650);
  }
  await sleep(2400); // the flushed batch with per-item results
  // one more rate burst to show it stays denied (bucket still empty), then reset.
  await page.getByRole("button", { name: "Reset" }).click();
  await sleep(700);
  await page.getByRole("button", { name: "Send" }).first().click();
  await sleep(1600);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
