// Screencast: Grocery Shop app (bundled React SPA + real WASI barcode decoder)
// Demonstrates:
// 1. Shopper Flow:
//    - Select Shopper role
//    - Scan real EAN-13 PNG barcode fixture (Organic Extra Virgin Olive Oil) -> WASI decodes 4006381333931
//    - Add to basket
//    - Scan real UPC-A PNG barcode fixture (Artisan Sourdough Loaf) -> WASI decodes 0036000291452
//    - Add to basket
//    - Open cart drawer, review subtotals, confirm and pay
//    - Display real receipt with updated inventory
// 2. Admin Flow:
//    - Switch to Inventory Admin view
//    - Review low-stock alert banner (Farm Fresh Whole Milk, stock: 3)
//    - Click "+10 Restock" -> stock increments to 13 and alert clears
//    - Admin intake scanner: upload Code-128 delivery barcode (ZZG4ZDMEN)
//    - Verify decoded digits and symbology
//    - View real-time inventory table with stock controls
//
// Prereq: comp-host serving grocery_domain.composed.wasm on http://127.0.0.1:3055

import { chromium } from "playwright";
import { mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const BASE = process.env.GROCERY_URL || "http://127.0.0.1:3055";
const OUT = join(__dirname, "videos", "grocery");
mkdirSync(OUT, { recursive: true });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: 414, height: 896 },
  recordVideo: { dir: OUT, size: { width: 414, height: 896 } },
  deviceScaleFactor: 2,
  isMobile: true,
  hasTouch: true,
});

const page = await ctx.newPage();

const safeClick = async (selector) => {
  await page.waitForSelector(selector, { state: "visible", timeout: 10000 });
  await page.evaluate((sel) => {
    const el = document.querySelector(sel);
    if (el) el.click();
    else throw new Error("Element not found: " + sel);
  }, selector);
};

try {
  console.log("Navigating to Holon Grocery React SPA on:", BASE);
  await page.goto(BASE, { waitUntil: "networkidle" });
  await sleep(1500);

  // ---- 1. Shopper Journey ----------------------------------------------------
  console.log("Activating Shopper Role...");
  await safeClick("#role-shopper");
  await sleep(800);

  // Scan EAN-13 Olive Oil
  console.log("Scanning EAN-13 barcode fixture (Organic Olive Oil)...");
  await safeClick("#test-ean13-btn");
  await page.waitForSelector("#scan-result-card", { state: "visible", timeout: 8000 });
  await sleep(1800);

  console.log("Adding scanned Olive Oil to basket...");
  await safeClick("#add-scanned-to-cart-btn");
  await sleep(1200);

  // Scan UPC-A Sourdough
  console.log("Scanning UPC-A barcode fixture (Artisan Sourdough)...");
  await safeClick("#test-upca-btn");
  await sleep(1800);

  console.log("Adding scanned Sourdough to basket...");
  await safeClick("#add-scanned-to-cart-btn");
  await sleep(1200);

  // Open Cart Modal (Bottom Sheet)
  console.log("Opening shopping cart bottom sheet...");
  await safeClick("#open-cart-btn");
  await page.waitForSelector(".mobile-bottom-sheet, .modal-content", { state: "visible", timeout: 6000 });
  await sleep(2200);

  // Confirm Checkout
  console.log("Confirming checkout transaction...");
  await safeClick("#checkout-btn");
  await page.waitForSelector("#dismiss-receipt-btn", { state: "visible", timeout: 6000 });
  await sleep(2500);

  // Dismiss Receipt
  console.log("Dismissing receipt modal...");
  await safeClick("#dismiss-receipt-btn");
  await sleep(1200);

  // ---- 2. Inventory Admin Journey --------------------------------------------
  console.log("Switching to Inventory Admin view...");
  await safeClick("#role-admin");
  await sleep(1500);

  // Review Low Stock Alert
  console.log("Reviewing low-stock alert banner...");
  await sleep(1500);

  // Restock Farm Fresh Milk (+10)
  console.log("Restocking Farm Fresh Milk by +10 units...");
  await page.evaluate(() => {
    const btn = document.querySelector("button[id^='restock-']");
    if (btn) btn.click();
  });
  await sleep(2000);

  // Admin Intake: test Code-128 fixture
  console.log("Admin scanning Code-128 intake barcode fixture...");
  await page.evaluate(() => {
    const btns = Array.from(document.querySelectorAll("button"));
    const intake = btns.find((b) => b.textContent && b.textContent.includes("Intake Code-128"));
    if (intake) intake.click();
  });
  await sleep(2000);

  // Scroll down smoothly to inspect Inventory List
  console.log("Reviewing real-time inventory cards...");
  await page.evaluate(() => window.scrollBy({ top: 360, behavior: "smooth" }));
  await sleep(1500);

  // Quick adjust an item (+5)
  console.log("Adjusting stock with quick +5 stepper...");
  await page.evaluate(() => {
    const btn = document.querySelector("button[id^='stock-inc-']");
    if (btn) btn.click();
  });
  await sleep(2000);

  console.log("Screencast recording completed successfully.");
} catch (err) {
  console.error("Screencast recording error:", err);
  throw err;
} finally {
  await page.close();
  await ctx.close();
  await browser.close();
}
