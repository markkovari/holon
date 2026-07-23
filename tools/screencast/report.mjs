// Screencast: batch CSV import/report. Paste a CSV with a mix of valid and
// invalid rows, import it, and watch typed validation split the set — clean
// rows land in the paged report, bad rows come back with per-field errors —
// then export the clean set back to CSV. The batch-ingest + round-trip axis in
// a real browser.
import { chromium } from "playwright";

const BASE = process.env.REPORT_URL || "http://127.0.0.1:3022";
const OUT = new URL("./videos/report/", import.meta.url).pathname;
const W = 960, H = 820;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
});
const page = await ctx.newPage();
await page.goto(BASE);

try {
  await sleep(1000); // sample CSV pre-filled

  // import the pre-filled sample: 3 clean, 2 rejected with per-field errors.
  await page.click("#import");
  await sleep(2400);

  // page the clean report through the opaque cursor.
  const more = page.locator("#more");
  if (await more.isVisible()) {
    await more.click();
    await sleep(1800);
  }

  // add a row that fixes one reject, re-import to show the split move.
  const fixed = `name,email,age,role
Ada Lovelace,ada@example.com,36,admin
Alan Turing,alan@example.com,41,user
Grace Hopper,grace@example.com,45,guest
Katherine Johnson,katherine@example.com,101,guest
Bad Email,not-an-email,30,user`;
  await page.fill("#csv", fixed);
  await sleep(800);
  await page.click("#import");
  await sleep(2400);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
