// Screencast: the books double-entry app (React + shadcn UI) on the REAL running
// app. Registers (seeding a demo chart + entries), posts a balanced journal
// entry via the double-entry editor (the badge flips to "balanced"), tries an
// unbalanced one (Post stays disabled), then shows the Reports — trial balance,
// P&L, and a balance sheet that BALANCES.
//
// Prereq: from repo root  `just host-books &`   (builds the UI, serves on :3045)
import { chromium } from "playwright";

const BASE = process.env.BOOKS_URL || "http://127.0.0.1:3045";
const OUT = new URL("./videos/books/", import.meta.url).pathname;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: 900, height: 760 },
  recordVideo: { dir: OUT, size: { width: 900, height: 760 } },
  deviceScaleFactor: 1,
});
const page = await ctx.newPage();
await page.goto(BASE);

try {
  await page.getByPlaceholder("email").fill("you@acme.io");
  await page.getByPlaceholder("password").fill("pw12345678");
  await page.getByRole("button", { name: "Register" }).click();
  await page.getByRole("tab", { name: "Journal" }).waitFor({ timeout: 10000 });
  await sleep(1000);

  // Build a balanced entry: Dr 1100 A/R 250 · Cr 4000 Sales 250.
  const combos = page.locator("button[role=combobox]");
  await page.locator("input[placeholder='memo']").fill("Client invoice");
  await combos.nth(0).click(); await sleep(300); await page.getByRole("option", { name: /1100/ }).click();
  const amts = page.locator("input[type=number]");
  await amts.nth(0).fill("250"); await sleep(800); // now debits 250, credits 0 -> not balanced
  await combos.nth(1).click(); await sleep(300); await page.getByRole("option", { name: /4000/ }).click();
  await amts.nth(1).fill("250"); await sleep(1400); // -> badge flips to "balanced"
  await page.getByRole("button", { name: "Post entry" }).click();
  await sleep(1600);

  // Reports: trial balance / P&L / balance sheet.
  await page.getByRole("tab", { name: "Reports" }).click();
  await sleep(2600);
  await page.mouse.wheel(0, 400); await sleep(2200);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
