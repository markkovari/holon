// Screencast: Acme Vet Clinic (the petclinic showcase). Signs in as a pet-owner,
// adds a pet, searches, and books an appointment — the role-scoped owner view of
// a ~20-component composed app on the native Rust host.
import { chromium } from "playwright";

const BASE = process.env.VET_URL || "http://127.0.0.1:3007";
const OUT = new URL("./videos/vet/", import.meta.url).pathname;
const W = 1200, H = 860;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
});
const page = await ctx.newPage();

try {
  await page.goto(BASE, { waitUntil: "networkidle" });
  await page.waitForSelector("#email");
  await sleep(1200);

  // Sign in as the pet-owner.
  await page.fill("#email", "owner@acme-vet.test");
  await sleep(300);
  await page.fill("#password", "ownerpass1");
  await sleep(400);
  await page.getByRole("button", { name: "Sign in" }).last().click();
  await page.waitForSelector("#pet-name", { timeout: 10000 });
  await sleep(1500);

  // Add a pet.
  await page.fill("#pet-name", "Luna");
  await sleep(300);
  await page.fill("#pet-species", "cat");
  await sleep(400);
  await page.getByRole("button", { name: "Add pet" }).click();
  await page.getByText("Luna", { exact: true }).first().waitFor({ timeout: 8000 });
  await sleep(1500);

  // Search.
  await page.fill("#pet-q", "Luna");
  await sleep(400);
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await sleep(1600);
  await page.getByRole("button", { name: "Clear" }).click();
  await sleep(1200);

  // Book an appointment: pick pet → date → book.
  await page.click("#appt-pet");
  await sleep(500);
  await page.getByRole("option").first().click();
  await sleep(600);
  await page.getByText("Pick a date & time").click();
  await sleep(700);
  await page.locator("[role=gridcell] button:not([disabled])").first().click();
  await sleep(700);
  await page.getByRole("button", { name: "Book" }).click();
  await sleep(1500);

  // Show the booked appointment row (bottom of the page).
  await page.evaluate(() => window.scrollTo({ top: document.body.scrollHeight, behavior: "smooth" }));
  await sleep(3200);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
