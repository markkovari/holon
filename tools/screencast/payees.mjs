// Screencast: the payees book (React + shadcn UI) on the REAL running app.
// Registers (seeding demo payees), types a tampered IBAN (a red "checksum
// failed" appears), fixes it (a green "Valid · NL · …" appears), adds the payee,
// then adds a second — the IBAN is validated by the composed iban:validate
// component as you type.
//
// Prereq: from repo root  `just host-payees &`   (builds the UI, serves on :3047)
import { chromium } from "playwright";

const BASE = process.env.PAYEES_URL || "http://127.0.0.1:3047";
const OUT = new URL("./videos/payees/", import.meta.url).pathname;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: 760, height: 820 },
  recordVideo: { dir: OUT, size: { width: 760, height: 820 } },
  deviceScaleFactor: 1,
});
const page = await ctx.newPage();
await page.goto(BASE);

async function typeSlow(loc, text) {
  await loc.fill("");
  for (const ch of text) { await loc.press(ch === " " ? "Space" : ch); await sleep(35); }
}

try {
  await page.getByPlaceholder("email").fill("you@acme.io");
  await page.getByPlaceholder("password").fill("pw12345678");
  await page.getByRole("button", { name: "Register" }).click();
  await page.getByText(/Payees \(/).waitFor({ timeout: 10000 });
  await sleep(900);

  const ib = page.getByPlaceholder(/IBAN/);
  await page.getByPlaceholder("Name").fill("Dutch Vendor");
  // a tampered IBAN -> red "checksum failed"
  await typeSlow(ib, "NL91 ABNA 0417 1643 01");
  await sleep(1600);
  // fix the last digit -> green "Valid"
  await ib.fill("NL91 ABNA 0417 1643 00");
  await sleep(1800);
  await page.getByRole("button", { name: "Add payee" }).click();
  await sleep(1400);

  // a second, valid one from another country
  await page.getByPlaceholder("Name").fill("Swiss Partner AG");
  await typeSlow(ib, "CH93 0076 2011 6238 5295 7");
  await sleep(1600);
  await page.getByRole("button", { name: "Add payee" }).click();
  await sleep(1800);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
