// Screencast: abtest A/B/n console. Two users (alice/bob) sit in different arms;
// drag control's weight up and watch the cohort grid re-bucket stickily; click
// tiles to convert them and watch the per-arm conversion-rate bars pull apart —
// sticky weighted assignment + live attribution over SSE, in a real browser.
import { chromium } from "playwright";

const BASE = process.env.ABTEST_URL || "http://127.0.0.1:3018";
const OUT = new URL("./videos/abtest/", import.meta.url).pathname;
const W = 1240, H = 760;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
});
const page = await ctx.newPage();
await page.goto(BASE);

const setWeight = async (arm, v) => {
  const s = page.locator(`[data-arm="${arm}"]`);
  await s.evaluate((el, val) => {
    el.value = val;
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
  }, String(v));
};

try {
  // wait for the seeded experiment + grid to render.
  await page.locator(".cell").first().waitFor({ timeout: 8000 });
  await sleep(1800); // show two users landing in different arms

  // shift control 50 -> 60: cohort re-buckets, but sticky (already-control stay;
  // alice/heidi keep their distinct arms). A gentle bump keeps the diff visible.
  await setWeight("control", 60);
  await sleep(2500);

  // convert a spread of subjects — click ~18 tiles across arms.
  const cells = page.locator(".cell");
  const count = await cells.count();
  for (let i = 0; i < 18; i++) {
    await cells.nth((i * 5) % count).click();
    await sleep(120);
  }
  await sleep(2500); // let the per-arm rate bars settle
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
