// Screencast: Grocery Shop app Mobile View (414x896)
// Demonstrates REAL AUTHENTICATION & ZERO FAKE ROLE TOGGLES:
// 1. Mobile Customer Sign In (shopper / shopper123)
// 2. Barcode Scanning + Mobile Bottom Sheet Cart + Checkout
// 3. Genuine Sign Out -> Session terminated
// 4. Registration of Store Admin account
// 5. Admin Command Center unlocked strictly via authenticated Admin session
// 6. User Management console, Restock, Intake scan, Stock stepper

import { chromium } from "playwright";
import { mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const BASE = process.env.GROCERY_URL || "http://127.0.0.1:3055";
const OUT = join(__dirname, "videos", "grocery-mobile");
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
  console.log("Navigating to Holon Grocery React SPA on mobile:", BASE);
  await page.goto(BASE, { waitUntil: "networkidle" });
  await sleep(1500);

  // Clear localStorage
  await page.evaluate(() => localStorage.clear());
  await page.reload({ waitUntil: "networkidle" });
  await sleep(1000);

  // ---- 1. REAL AUTH: Mobile Customer Sign In ---------------------------------
  console.log("Mobile Auth: Opening Sign In modal...");
  await safeClick("#header-signin-btn");
  await page.waitForSelector("#submit-login-btn", { state: "visible", timeout: 6000 });
  await sleep(1000);

  console.log("Mobile Auth: Filling Shopper credentials...");
  await safeClick("#demo-shopper-login-btn");
  await sleep(800);

  console.log("Mobile Auth: Submitting Sign In to WASI backend...");
  await safeClick("#submit-login-btn");
  await page.waitForSelector("#user-session-pill", { state: "visible", timeout: 6000 });
  await sleep(1500);

  // ---- 2. Mobile Shopper Journey ---------------------------------------------
  console.log("Mobile Shopper: Scanning EAN-13 Olive Oil...");
  await safeClick("#test-ean13-btn");
  await page.waitForSelector("#scan-result-card", { state: "visible", timeout: 8000 });
  await sleep(1400);

  console.log("Mobile Shopper: Adding scanned Olive Oil to basket...");
  await safeClick("#add-scanned-to-cart-btn");
  await sleep(1000);

  console.log("Mobile Shopper: Scanning UPC-A Sourdough...");
  await safeClick("#test-upca-btn");
  await sleep(1400);

  console.log("Mobile Shopper: Adding scanned Sourdough to basket...");
  await safeClick("#add-scanned-to-cart-btn");
  await sleep(1000);

  // Open Cart Modal (Bottom Sheet via floating dock)
  console.log("Mobile Shopper: Opening cart bottom sheet...");
  await safeClick("#open-cart-btn");
  await page.waitForSelector(".mobile-bottom-sheet", { state: "visible", timeout: 6000 });
  await sleep(1800);

  // Confirm Checkout
  console.log("Mobile Shopper: Confirming checkout...");
  await safeClick("#checkout-btn");
  await page.waitForSelector("#dismiss-receipt-btn", { state: "visible", timeout: 6000 });
  await sleep(2000);

  // Dismiss Receipt
  console.log("Mobile Shopper: Dismissing receipt...");
  await safeClick("#dismiss-receipt-btn");
  await sleep(1200);

  // ---- 3. REAL AUTH: Sign Out ------------------------------------------------
  console.log("Mobile Auth: Shopper signing out...");
  await safeClick("#header-logout-btn");
  await page.waitForSelector("#header-signin-btn", { state: "visible", timeout: 6000 });
  await sleep(1400);

  // ---- 4. REAL AUTH: Register Store Admin -----------------------------------
  console.log("Mobile Auth: Registering new Store Admin...");
  await safeClick("#header-register-btn");
  await page.waitForSelector("#reg-role-admin", { state: "visible", timeout: 6000 });
  await sleep(1000);

  console.log("Mobile Auth: Selecting Store Admin role group...");
  await safeClick("#reg-role-admin");
  await sleep(600);

  const uniqueAdmin = "mobile_admin_" + Math.floor(Math.random() * 1000);
  await page.fill("#register-username", uniqueAdmin);
  await sleep(300);
  await page.fill("#register-name", "Mobile Admin");
  await sleep(300);
  await page.fill("#register-password", "admin123");
  await sleep(800);

  console.log("Mobile Auth: Submitting Admin registration...");
  await safeClick("#submit-register-btn");
  // Admin Command Center unlocks
  await page.waitForSelector("#admin-tab-users", { state: "visible", timeout: 8000 });
  await sleep(2000);

  // ---- 5. Mobile Admin Journey -----------------------------------------------
  console.log("Mobile Admin: Navigating to User Management & RBAC...");
  await safeClick("#admin-tab-users");
  await page.waitForSelector("#admin-user-management", { state: "visible", timeout: 6000 });
  await sleep(2200);

  console.log("Mobile Admin: Returning to Catalog & Inventory...");
  await safeClick("#admin-tab-inventory");
  await sleep(1200);

  // Restock low-stock item (+10)
  console.log("Mobile Admin: Restocking low-stock item (+10)...");
  await page.evaluate(() => {
    const btn = document.querySelector("button[id^='restock-']");
    if (btn) btn.click();
  });
  await sleep(1500);

  // Admin Intake: test Code-128 fixture
  console.log("Mobile Admin: Scanning Code-128 delivery manifest fixture...");
  await page.evaluate(() => {
    const btns = Array.from(document.querySelectorAll("button"));
    const intake = btns.find((b) => b.textContent && b.textContent.includes("Intake Code-128"));
    if (intake) intake.click();
  });
  await sleep(1500);

  // Scroll smoothly to inspect Inventory List
  console.log("Mobile Admin: Scrolling down to real-time inventory...");
  await page.evaluate(() => window.scrollBy({ top: 360, behavior: "smooth" }));
  await sleep(1400);

  // Quick adjust an item (+5)
  console.log("Mobile Admin: Adjusting stock with +5 stepper...");
  await page.evaluate(() => {
    const btn = document.querySelector("button[id^='stock-inc-']");
    if (btn) btn.click();
  });
  await sleep(2000);

  console.log("Mobile screencast completed successfully.");
} catch (err) {
  console.error("Mobile screencast error:", err);
  throw err;
} finally {
  await page.close();
  await ctx.close();
  await browser.close();
}
