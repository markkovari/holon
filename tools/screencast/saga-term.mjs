// Screencast: the saga is API-only, so we film its story as a terminal clip —
// the commit path, a compensation (rollback), and the durability restart proof.
import { chromium } from "playwright";
const HTML = "file://" + new URL("./saga-term.html", import.meta.url).pathname;
const OUT = new URL("./videos/saga/", import.meta.url).pathname;
const W = 1000, H = 720;

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
  deviceScaleFactor: 2,
});
const page = await ctx.newPage();
await page.goto(HTML);
await page.waitForFunction(() => window.__done === true, { timeout: 40000 });
await page.waitForTimeout(2500);
await ctx.close();
await browser.close();
console.log("done");
