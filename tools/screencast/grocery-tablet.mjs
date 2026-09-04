// Screencast: Grocery Shop app Tablet View (768x960)
// Demonstrates REAL AUTHENTICATION & ZERO FAKE ROLE TOGGLES:
// 1. Customer Sign In (shopper / shopper123)
// 2. Barcode Scanning + Cart + Checkout
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
const OUT = join(__dirname, "videos", "grocery-tablet");
mkdirSync(OUT, { recursive: true });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: 768, height: 960 },
  recordVideo: { dir: OUT, size: { width: 768, height: 960 } },
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
  console.log("Navigating to Holon Grocery React SPA on tablet:", BASE);
  await page.goto(BASE, { waitUntil: "networkidle" });
  await sleep(1500);

  // Clear localStorage
  await page.evaluate(() => localStorage.clear());
  await page.reload({ waitUntil: "networkidle" });
  await sleep(1000);

  // ---- 1. REAL AUTH: Customer Sign In ----------------------------------------
  console.log("Tablet Auth: Opening Sign In modal...");
  await safeClick("#header-signin-btn");
  await page.waitForSelector("#submit-login-btn", { state: "visible", timeout: 6000 });
  await sleep(1000);

  console.log("Tablet Auth: Filling Shopper credentials...");
  await safeClick("#demo-shopper-login-btn");
  await sleep(800);

  console.log("Tablet Auth: Submitting Sign In to WASI backend...");
  await safeClick("#submit-login-btn");
  await page.waitForSelector("#user-session-pill", { state: "visible", timeout: 6000 });
  await sleep(1500);

  // ---- 2. Tablet Shopper Shopping & Checkout ---------------------------------
  console.log("Tablet Shopper: Scanning EAN-13 Olive Oil...");
  await safeClick("#test-ean13-btn");
  await page.waitForSelector("#scan-result-card", { state: "visible", timeout: 8000 });
  await sleep(1400);

  console.log("Tablet Shopper: Adding scanned Olive Oil to basket...");
  await safeClick("#add-scanned-to-cart-btn");
  await sleep(1000);

  console.log("Tablet Shopper: Scanning UPC-A Sourdough...");
  await safeClick("#test-upca-btn");
  await sleep(1400);

  console.log("Tablet Shopper: Adding scanned Sourdough to basket...");
  await safeClick("#add-scanned-to-cart-btn");
  await sleep(1000);

  // Open Cart Modal (bottom dock)
  console.log("Tablet Shopper: Opening cart modal...");
  await safeClick("#open-cart-btn");
  await page.waitForSelector(".mobile-bottom-sheet, .modal-content", { state: "visible", timeout: 6000 });
  await sleep(1800);

  // Confirm Checkout
  console.log("Tablet Shopper: Confirming checkout...");
  await safeClick("#checkout-btn");
  await page.waitForSelector("#dismiss-receipt-btn", { state: "visible", timeout: 6000 });
  await sleep(2000);

  // Dismiss Receipt
  console.log("Tablet Shopper: Dismissing receipt...");
  await safeClick("#dismiss-receipt-btn");
  await sleep(1200);

  // ---- 3. REAL AUTH: Sign Out ------------------------------------------------
  console.log("Tablet Auth: Shopper signing out...");
  await safeClick("#header-logout-btn");
  await page.waitForSelector("#header-signin-btn", { state: "visible", timeout: 6000 });
  await sleep(1400);

  // ---- 4. REAL AUTH: Register Store Admin -----------------------------------
  console.log("Tablet Auth: Registering new Store Admin...");
  await safeClick("#header-register-btn");
  await page.waitForSelector("#reg-role-admin", { state: "visible", timeout: 6000 });
  await sleep(1000);

  console.log("Tablet Auth: Selecting Store Admin role group...");
  await safeClick("#reg-role-admin");
  await sleep(600);

  const uniqueAdmin = "tablet_admin_" + Math.floor(Math.random() * 1000);
  await page.fill("#register-username", uniqueAdmin);
  await sleep(300);
  await page.fill("#register-name", "Tablet Admin");
  await sleep(300);
  await page.fill("#register-password", "admin123");
  await sleep(800);

  console.log("Tablet Auth: Submitting Admin registration...");
  await safeClick("#submit-register-btn");
  // Admin Command Center unlocks
  await page.waitForSelector("#admin-tab-users", { state: "visible", timeout: 8000 });
  await sleep(2000);

  // ---- 5. Tablet Admin Flow --------------------------------------------------
  console.log("Tablet Admin: Navigating to User Management & RBAC...");
  await safeClick("#admin-tab-users");
  await page.waitForSelector("#admin-user-management", { state: "visible", timeout: 6000 });
  await sleep(2200);

  console.log("Tablet Admin: Returning to Catalog & Inventory...");
  await safeClick("#admin-tab-inventory");
  await sleep(1200);

  // Restock low-stock item (+10)
  console.log("Tablet Admin: Restocking low-stock item (+10)...");
  await page.evaluate(() => {
    const btn = document.querySelector("button[id^='restock-']");
    if (btn) btn.click();
  });
  await sleep(1500);

  // Admin Intake: scan Code-128
  console.log("Tablet Admin: Scanning Code-128 intake fixture...");
  await page.evaluate(() => {
    const btns = Array.from(document.querySelectorAll("button"));
    const intake = btns.find((b) => b.textContent && b.textContent.includes("Intake Code-128"));
    if (intake) intake.click();
  });
  await sleep(1500);

  // Smooth scroll to view real-time inventory
  console.log("Tablet Admin: Scrolling to real-time inventory...");
  await page.evaluate(() => window.scrollBy({ top: 380, behavior: "smooth" }));
  await sleep(1400);

  // Quick adjust in right pane (+5)
  console.log("Tablet Admin: Stepping stock (+5)...");
  await page.evaluate(() => {
    const btn = document.querySelector("button[id^='stock-inc-']");
    if (btn) btn.click();
  });
  await sleep(2000);

  console.log("Tablet screencast completed successfully.");
} catch (err) {
  console.error("Tablet screencast error:", err);
  throw err;
} finally {
  await page.close();
  await ctx.close();
  await browser.close();
}
