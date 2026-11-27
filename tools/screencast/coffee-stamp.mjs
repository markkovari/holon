// Screencast: Roast & Ritual Coffee — QR Stamp Card loyalty app.
// Customer opens card, barista mints single-use Stamp QRs, customer scans until
// the punch card fills up and unlocks a Free Coffee Voucher QR, then barista redeems it.
import { chromium } from "playwright";

const BASE = process.env.COFFEE_URL || "http://127.0.0.1:3092";
const OUT = new URL("./videos/coffee-stamp/", import.meta.url).pathname;
const W = 860, H = 760;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
});
const page = await ctx.newPage();

try {
  await page.goto(BASE, { waitUntil: "networkidle" });
  await page.waitForSelector("#custName");
  await sleep(900);

  // 1. Customer opens their loyalty stamp card.
  await page.fill("#custName", "Maya");
  await sleep(250);
  await page.fill("#custEmail", "maya@coffee.test");
  await sleep(350);
  await page.getByRole("button", { name: "Open Card" }).click();
  await page.waitForSelector("#punchGrid .slot");
  await sleep(1100);

  // 2. Barista mints a 2-stamp QR for Oat Flat White.
  await page.click("#tabBarista");
  await page.waitForSelector("#drinkSelect");
  await sleep(600);
  await page.selectOption("#drinkSelect", "Oat Flat White");
  await sleep(250);
  await page.selectOption("#stampCount", "2");
  await sleep(300);
  await page.getByRole("button", { name: "Generate Stamp QR" }).click();
  await page.waitForSelector("#issuedQrSvg svg");
  await sleep(1400);

  // 3. Customer scans the QR (+2 stamps -> 2/4).
  await page.getByRole("button", { name: /Simulate Customer Scan/ }).click();
  await page.waitForSelector("#punchGrid .slot.filled");
  await sleep(1400);

  // 4. Barista mints another 2-stamp QR for Iced Caramel Latte.
  await page.click("#tabBarista");
  await sleep(500);
  await page.selectOption("#drinkSelect", "Iced Caramel Latte");
  await sleep(250);
  await page.selectOption("#stampCount", "2");
  await sleep(300);
  await page.getByRole("button", { name: "Generate Stamp QR" }).click();
  await page.waitForSelector("#issuedQrSvg svg");
  await sleep(1100);

  // 5. Customer scans -> 4/4 reached -> Free Coffee Voucher + QR unlocked!
  await page.getByRole("button", { name: /Simulate Customer Scan/ }).click();
  await page.waitForSelector("#voucherList .voucher code");
  await sleep(700);
  await page.evaluate(() => window.scrollTo({ top: document.body.scrollHeight, behavior: "smooth" }));
  await sleep(2000);

  // Read the unlocked voucher code.
  const voucherCode = (await page.locator("#voucherList .voucher code").first().textContent()).trim();

  // 6. Barista redeems the Free Coffee Voucher.
  await page.evaluate(() => window.scrollTo({ top: 0, behavior: "instant" }));
  await page.click("#tabBarista");
  await sleep(600);
  await page.evaluate(() => window.scrollTo({ top: document.body.scrollHeight, behavior: "smooth" }));
  await sleep(400);
  await page.fill("#redeemCodeInput", voucherCode);
  await sleep(500);
  await page.getByRole("button", { name: "Redeem Free Coffee" }).click();
  await sleep(800);
  await page.evaluate(() => window.scrollTo({ top: 0, behavior: "smooth" }));
  await sleep(2000);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
