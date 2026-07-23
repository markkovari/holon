// Screencast: a paste/gist bin. Paste Markdown containing an email + a card
// number, submit, and watch the PII masked at ingest (a "N PII masked" badge)
// and the Markdown rendered to safe HTML with a raw <script> escaped. Then a
// second paste with the same title to show slug de-duplication. The pure-compute
// pipeline axis in a real browser.
import { chromium } from "playwright";

const BASE = process.env.PASTE_URL || "http://127.0.0.1:3024";
const OUT = new URL("./videos/paste/", import.meta.url).pathname;
const W = 900, H = 720;
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
  // paste the pre-filled sample (email + card + <script> + markdown).
  await page.click("#save");
  await sleep(2600); // render + badge appear

  // a second paste, same title -> slug de-dupes to my-notes-2 style.
  await page.fill("#title", "Deploy notes");
  await page.fill("#body", "# Second\n\nAnother note, no secrets here.\n\n- clean");
  await sleep(600);
  await page.click("#save");
  await sleep(2400);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
