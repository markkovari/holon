// Screencast: status page / uptime monitor. Seed two monitors over the API (one
// self-probe that stays up, one dead target), then click "Run checks" across
// two periods and watch the dead one walk up -> degraded -> down while the
// healthy one stays green. The timer-driven axis in a real browser.
import { chromium } from "playwright";

const BASE = process.env.STATUS_URL || "http://127.0.0.1:3012";
const OUT = new URL("./videos/status/", import.meta.url).pathname;
const W = 720, H = 520;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
});
const page = await ctx.newPage();

// seed the monitors over the API before filming (the demo page has no add form).
const add = (name, url) =>
  page.request.post(`${BASE}/api/monitors`, { data: { name, url, period: 10 } });
await add("api.example", `${BASE}/`);            // probes our own root -> 200 -> up
await add("db.internal", "http://127.0.0.1:59999/"); // dead port -> fails

await page.goto(BASE);

try {
  await sleep(1000);
  // tick 1: self stays up, dead goes degraded.
  await page.click("text=Run checks");
  await sleep(2400);
  // wait out the 10s period, tick 2: dead goes down.
  await sleep(10500);
  await page.click("text=Run checks");
  await sleep(2600);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
