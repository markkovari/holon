// Screencast: the durable job queue board, live on the REAL running app. A burst
// of jobs marches Queued → Running → Done; a flaky job retries with backoff (its
// attempt count climbs) then succeeds; a boom job exhausts its attempts and lands
// in the Dead-letter column; then Replay requeues it. The SSE board self-ticks,
// so jobs advance on their own once enqueued.
//
// Prereq: from repo root  `just host-jobs &`   (serves on :3038; max-attempts=2,
// base-backoff=1s so retries/DLQ happen within the clip).
import { chromium } from "playwright";

const BASE = process.env.JOBS_URL || "http://127.0.0.1:3038";
const OUT = new URL("./videos/jobs/", import.meta.url).pathname;
const W = 1200, H = 680;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
  deviceScaleFactor: 2,
});
const page = await ctx.newPage();
await page.goto(BASE);

try {
  await page.locator("#status").filter({ hasText: "live" }).waitFor({ timeout: 10000 });
  await sleep(700);

  // 1. a burst of ordinary jobs -> march to Done
  await page.click("#burst");
  await sleep(2600);

  // 2. a flaky job -> retries with backoff (attempt count climbs), then done
  await page.click('button[data-type="flaky"]');
  await sleep(4200);

  // 3. a boom job -> exhausts attempts -> Dead-letter
  await page.click('button[data-type="boom"]');
  await page.locator("#c-dead .card").first().waitFor({ timeout: 12000 });
  await sleep(1200);

  // 4. replay the dead job -> back to Queued -> runs again
  await page.locator("#c-dead button[data-replay]").first().click();
  await sleep(3000);

  // hold on the final board
  await sleep(1500);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
