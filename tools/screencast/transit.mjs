// Screencast: the transit ticketing app (React + shadcn UI) on the REAL running
// app, at a PHONE viewport. A rider buys a fare and shows its QR; then a
// validator signs in and validates a ticket id (via the manual field — a
// headless browser has no camera) to a big green ACCEPTED, and re-scans the same
// single ticket to a red REJECTED "already used".
//
// Prereq: from repo root  `just host-transit &`   (builds the UI, serves on :3042)
import { chromium } from "playwright";

const BASE = process.env.TRANSIT_URL || "http://127.0.0.1:3042";
const OUT = new URL("./videos/transit/", import.meta.url).pathname;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const HJ = { "content-type": "application/json" };
async function api(path, method = "GET", body, token) {
  const h = { ...HJ }; if (token) h.authorization = `Bearer ${token}`;
  const r = await fetch(`${BASE}/api${path}`, { method, headers: h, body: body ? JSON.stringify(body) : undefined });
  return r.json().catch(() => ({}));
}
async function signup(email, role) {
  await api("/register", "POST", { email, password: "pw12345678", role });
  return (await api("/login", "POST", { email, password: "pw12345678" })).access_token;
}

// seed accounts + a single ticket the validator will scan.
const rt = await signup("rider@acme.io", "rider");
await signup("insp@acme.io", "validator");
const scanId = (await api("/tickets", "POST", { fare: "single" }, rt)).id;

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: 414, height: 896 },
  recordVideo: { dir: OUT, size: { width: 414, height: 896 } },
  deviceScaleFactor: 2, isMobile: true, hasTouch: true,
});
const page = await ctx.newPage();
await page.goto(BASE);

try {
  // ---- rider: buy a fare, show its QR ----
  await page.getByPlaceholder("email").fill("rider@acme.io");
  await page.getByPlaceholder("password").fill("pw12345678");
  await page.getByRole("button", { name: "Log in" }).click();
  await page.getByRole("tab", { name: "Buy" }).waitFor({ timeout: 10000 });
  await sleep(1000);
  // buy the 60-minute ticket (2nd Buy button), lands on My tickets.
  await page.getByRole("button", { name: "Buy" }).nth(1).click();
  await sleep(1800);
  await page.getByRole("button", { name: /Show/ }).first().click();
  await sleep(2600); // the big QR

  // ---- switch to the validator ----
  await page.getByTitle("Log out").click();
  await sleep(700);
  await page.getByPlaceholder("email").fill("insp@acme.io");
  await page.getByPlaceholder("password").fill("pw12345678");
  await page.getByRole("button", { name: "Log in" }).click();
  await page.getByPlaceholder("ticket id…").waitFor({ timeout: 10000 });
  await sleep(1200); // the camera scanner surface

  // validate the seeded single ticket -> ACCEPTED
  await page.getByPlaceholder("ticket id…").fill(scanId);
  await page.getByRole("button", { name: "Validate" }).click();
  await sleep(2600);
  // scan the same single ticket again -> REJECTED (already used)
  await page.getByPlaceholder("ticket id…").fill(scanId);
  await sleep(3200); // clear the client debounce
  await page.getByRole("button", { name: "Validate" }).click();
  await sleep(2600);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
