// Screencast: the throttle wall. Burst requests to drive the attempt bar to its
// ceiling and flip the key to LOCKED with a countdown, watch the quota gauge
// drain, then Reset to reopen — backpressure made visible, live over SSE.
import { chromium } from "playwright";

const BASE = process.env.RATELIMIT_URL || "http://127.0.0.1:3020";
const OUT = new URL("./videos/ratelimit/", import.meta.url).pathname;
const W = 820, H = 760;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
});
const page = await ctx.newPage();
await page.goto(BASE);

try {
  await sleep(1000);

  // a burst climbs the attempt bar + drains quota.
  await page.click("#burst");
  await sleep(2000);

  // a few single hits push it to the ceiling → LOCKED.
  for (let i = 0; i < 4; i++) { await page.click("#hit"); await sleep(350); }
  await sleep(2200); // hold on the LOCKED badge + countdown

  // reset re-opens the wall.
  await page.click("#reset");
  await sleep(2000);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
