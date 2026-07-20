// Screencast: conduit has no UI (it's API-only), so we film its actual proof —
// the official RealWorld Hurl conformance suite going green. The terminal HTML
// streams the REAL captured output (13/13 files, 154 requests).
import { chromium } from "playwright";
const HTML = "file://" + new URL("./conformance-term.html", import.meta.url).pathname;
const OUT = new URL("./videos/conduit/", import.meta.url).pathname;
const W = 980, H = 640;

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
  deviceScaleFactor: 2,
});
const page = await ctx.newPage();
await page.goto(HTML);
await page.waitForFunction(() => window.__done === true, { timeout: 30000 });
await page.waitForTimeout(2500); // hold on the final banner
await ctx.close();
await browser.close();
console.log("done");
