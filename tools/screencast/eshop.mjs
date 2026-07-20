// Screencast: eShop on wasmCloud. Drives the vanilla-JS storefront the whole
// way — sign in, add items, checkout — then lingers while the page's own 2s
// heartbeat pumps the order choreography (submitted → … → shipped).
import { chromium } from "playwright";

const BASE = process.env.ESHOP_URL || "http://127.0.0.1:3100";
const OUT = new URL("./videos/eshop/", import.meta.url).pathname;
const W = 1160, H = 820;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
  deviceScaleFactor: 1,
});
const page = await ctx.newPage();

try {
  await page.goto(BASE, { waitUntil: "networkidle" });
  await page.waitForSelector("#catalog .card", { timeout: 15000 });
  await sleep(1200);

  // Sign in (creds prefilled; the app auto-registers an unknown demo account).
  await page.click("#loginBtn");
  await page.waitForSelector("#basketPanel:not([hidden])", { timeout: 10000 });
  await sleep(1500);

  // Add two products once the catalog re-renders with enabled buttons.
  await page.waitForSelector("#catalog .card button:not([disabled])", { timeout: 8000 });
  for (const n of [0, 2]) {
    await page.getByRole("button", { name: "Add to basket" }).nth(n).click();
    await sleep(1600);
  }

  // Checkout (address + card prefilled).
  await page.locator("#checkoutBox").evaluate((el) => (el.open = true));
  await sleep(900);
  await page.click("#checkoutBtn");
  await sleep(1200);

  // Scroll to "My orders" — the page pumps the choreography every 2s, so the
  // order status chip advances (submitted → … → shipped) while we watch.
  await page.locator("#ordersPanel").scrollIntoViewIfNeeded();
  await sleep(12000);
} finally {
  await ctx.close(); // flushes the video
  await browser.close();
}
console.log("done");
