// Screencast: Grocery Shop app Desktop View (1200x750)
// Demonstrates REAL AUTHENTICATION & ZERO FAKE ROLE TOGGLES:
// 1. Customer Sign In (shopper / shopper123) -> Authenticated as Alex Shopper (SHOPPER role)
// 2. Barcode Scanning (EAN-13, UPC-A), Basket, and WASI Checkout Receipt
// 3. Genuine Sign Out -> Session terminated, Guest mode
// 4. Registration of Store Admin account (Maria Rodriguez, role: admin)
// 5. Admin Command Center unlocked strictly via authenticated Admin session
// 6. User Management console, Low-stock restock, delivery intake scan, stock stepper
import { chromium } from "playwright";
import { mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const BASE = process.env.GROCERY_URL || "http://127.0.0.1:3055";
const OUT = join(__dirname, "videos", "grocery-desktop");
mkdirSync(OUT, { recursive: true });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: 1200, height: 750 },
  recordVideo: { dir: OUT, size: { width: 1200, height: 750 } },
  deviceScaleFactor: 2,
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
  console.log("Navigating to Holon Grocery React SPA on desktop:", BASE);
  await page.goto(BASE, { waitUntil: "networkidle" });
  await sleep(1500);

  // Clear any existing localStorage session so we start clean
  await page.evaluate(() => localStorage.clear());
  await page.reload({ waitUntil: "networkidle" });
  await sleep(1000);

  // ---- 1. REAL AUTH: Customer Sign In ----------------------------------------
  console.log("Auth: Opening Sign In modal...");
  await safeClick("#header-signin-btn");
  await page.waitForSelector("#submit-login-btn", { state: "visible", timeout: 6000 });
  await sleep(1000);

  console.log("Auth: Filling Shopper customer credentials (shopper / shopper123)...");
  await safeClick("#demo-shopper-login-btn");
  await sleep(800);

  console.log("Auth: Submitting Sign In to WASI backend...");
  await safeClick("#submit-login-btn");
  await page.waitForSelector("#user-session-pill", { state: "visible", timeout: 6000 });
  await sleep(1500);

  // ---- 2. Shopper Shopping & Checkout ----------------------------------------
  console.log("Shopper: Scanning EAN-13 Olive Oil fixture...");
  await safeClick("#test-ean13-btn");
  await page.waitForSelector("#scan-result-card", { state: "visible", timeout: 8000 });
  await sleep(1400);

  console.log("Shopper: Adding scanned Olive Oil to basket...");
  await safeClick("#add-scanned-to-cart-btn");
  await sleep(1000);

  console.log("Shopper: Scanning UPC-A Sourdough fixture...");
  await safeClick("#test-upca-btn");
  await sleep(1400);

  console.log("Shopper: Adding scanned Sourdough to basket...");
  await safeClick("#add-scanned-to-cart-btn");
  await sleep(1000);

  // Open Cart Modal
  console.log("Shopper: Opening cart modal...");
  await safeClick("#header-open-cart-btn");
  await page.waitForSelector(".mobile-bottom-sheet, .modal-content", { state: "visible", timeout: 6000 });
  await sleep(1800);

  // Confirm Checkout
  console.log("Shopper: Confirming checkout transaction...");
  await safeClick("#checkout-btn");
  await page.waitForSelector("#dismiss-receipt-btn", { state: "visible", timeout: 6000 });
  await sleep(2000);

  // Dismiss Receipt
  console.log("Shopper: Dismissing receipt...");
  await safeClick("#dismiss-receipt-btn");
  await sleep(1200);

  // ---- 3. REAL AUTH: Sign Out ------------------------------------------------
  console.log("Auth: Shopper signing out (terminating session)...");
  await safeClick("#header-logout-btn");
  await page.waitForSelector("#header-signin-btn", { state: "visible", timeout: 6000 });
  await sleep(1400);

  // ---- 4. REAL AUTH: Register Store Admin Account ----------------------------
  console.log("Auth: Registering new Store Admin account...");
  await safeClick("#header-register-btn");
  await page.waitForSelector("#reg-role-admin", { state: "visible", timeout: 6000 });
  await sleep(1000);

  console.log("Auth: Selecting Store Admin role group...");
  await safeClick("#reg-role-admin");
  await sleep(600);

  const uniqueAdmin = "maria_admin_" + Math.floor(Math.random() * 1000);
  await page.fill("#register-username", uniqueAdmin);
  await sleep(300);
  await page.fill("#register-name", "Maria Rodriguez");
  await sleep(300);
  await page.fill("#register-password", "maria123");
  await sleep(800);

  console.log("Auth: Submitting Admin registration to WASI backend...");
  await safeClick("#submit-register-btn");
  // Admin Command Center unlocks because user.role === "admin"
  await page.waitForSelector("#admin-tab-users", { state: "visible", timeout: 8000 });
  await sleep(2000);

  // ---- 5. Admin Console & User Management -----------------------------------
  console.log("Admin: Inspecting User Management & RBAC Console...");
  await safeClick("#admin-tab-users");
  await page.waitForSelector("#admin-user-management", { state: "visible", timeout: 6000 });
  await sleep(2200);

  console.log("Admin: Returning to Catalog & Inventory...");
  await safeClick("#admin-tab-inventory");
  await sleep(1200);

  // Restock Farm Fresh Milk (+10)
  console.log("Admin: Restocking low-stock item (+10)...");
  await page.evaluate(() => {
    const btn = document.querySelector("button[id^='restock-']");
    if (btn) btn.click();
  });
  await sleep(1500);

  // Admin Intake: scan Code-128
  console.log("Admin: Scanning Code-128 intake fixture...");
  await page.evaluate(() => {
    const btns = Array.from(document.querySelectorAll("button"));
    const intake = btns.find((b) => b.textContent && b.textContent.includes("Intake Code-128"));
    if (intake) intake.click();
  });
  await sleep(1500);

  // Quick adjust in right pane (+5)
  console.log("Admin: Stepping stock (+5)...");
  await page.evaluate(() => {
    const btn = document.querySelector("button[id^='stock-inc-']");
    if (btn) btn.click();
  });
  await sleep(2000);

  console.log("Desktop screencast completed successfully.");
} catch (err) {
  console.error("Desktop screencast error:", err);
  throw err;
} finally {
  await page.close();
  await ctx.close();
  await browser.close();
}
