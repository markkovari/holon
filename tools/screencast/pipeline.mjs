// Screencast: pipeline reliable delivery. Enqueue a burst and watch cards march
// Pending → In-flight → Done live over SSE; take the sink DOWN and watch events
// retry then fall into the dead-letter tray; Replay one and watch it deliver —
// the retry/backoff/DLQ/replay story end to end in a real browser.
import { chromium } from "playwright";

const BASE = process.env.PIPELINE_URL || "http://127.0.0.1:3016";
const OUT = new URL("./videos/pipeline/", import.meta.url).pathname;
const W = 1200, H = 760;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
});
const page = await ctx.newPage();
await page.goto(BASE);

const setTopic = async (t) => { await page.fill("#topic", t); };

try {
  // wait for the board's SSE to connect (the ": connected" opens the stream).
  await sleep(1500);

  // ---- happy path: a burst marches to Done -------------------------------
  await setTopic("invoice.paid");
  await page.click("#burst");
  // let them flow through In-flight into Done.
  await page.locator(".lane.done .card").first().waitFor({ timeout: 8000 });
  await sleep(2200);

  // ---- sink DOWN: retries → dead-letter ----------------------------------
  await page.click("#sink"); // up → DOWN
  await sleep(800);
  await setTopic("charge.capture");
  await page.click("#burst");
  // wait for the dead-letter tray to populate.
  await page.locator("[data-replay]").first().waitFor({ timeout: 12000 });
  await sleep(2200);

  // ---- sink back UP + Replay a dead event --------------------------------
  await page.click("#sink"); // DOWN → up
  await sleep(700);
  await page.locator("[data-replay]").first().click();
  await sleep(3000);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
