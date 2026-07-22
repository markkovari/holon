// Screencast: flags live rollout. Add a flag, drag it to 30% and watch ~30 of
// 100 subject tiles light instantly; raise to 60% and watch MORE light with none
// turning off (sticky, monotone cohorts); trip the kill-switch and all go dark —
// runtime config propagation over SSE, in a real browser.
import { chromium } from "playwright";

const BASE = process.env.FLAGS_URL || "http://127.0.0.1:3017";
const OUT = new URL("./videos/flags/", import.meta.url).pathname;
const W = 1200, H = 720;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
});
const page = await ctx.newPage();
await page.goto(BASE);

const slider = () => page.locator('[data-slider="new-checkout"]');
const setSlider = async (v) => {
  // set the range value + dispatch change so the console POSTs the rule.
  await slider().evaluate((el, val) => {
    el.value = val;
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
  }, String(v));
};

try {
  await sleep(1200);

  // add a flag (starts at 0% — all dark).
  await page.fill("#newflag", "new-checkout");
  await page.click("#add");
  await sleep(1500);

  // 30% — ~30 tiles light, sticky.
  await setSlider(30);
  await sleep(2500);

  // 60% — more light, none turn off (monotone cohort).
  await setSlider(60);
  await sleep(2500);

  // kill-switch — all dark at once.
  await page.locator('[data-set="new-checkout"][data-rule="off"]').click();
  await sleep(2500);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
