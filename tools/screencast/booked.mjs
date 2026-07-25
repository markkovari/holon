// Screencast: the booked scheduling app (React + shadcn UI) on the REAL running
// app, recorded at a PHONE viewport. Seeds a resource + weekly availability via
// the API, then drives the SPA as an owner: the Book tab picks a weekday and
// shows free slots, taps one to book it (a confirmation card with the rendered
// email + an "Add to calendar" .ics), toggles weekly-repeat to book several
// weeks at once, the My bookings tab lists them, and Manage shows the weekly
// availability grid.
//
// Prereq: from repo root  `just host-booked &`   (builds the UI, serves on :3041)
import { chromium } from "playwright";

const BASE = process.env.BOOKED_URL || "http://127.0.0.1:3041";
const OUT = new URL("./videos/booked/", import.meta.url).pathname;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---- seed via the API -------------------------------------------------------
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

const owner = await signup("owner@acme.io", "owner");
await signup("mem@acme.io", "member");
// one resource, available Mon–Fri 09:00–17:00 (30-min slots).
const rid = (await api("/resources", "POST", { key: "room-a", name: "Room A", slot: 30 }, owner)).id;
const windows = [0, 1, 2, 3, 4].map((weekday) => ({ weekday, start: 9 * 60, end: 17 * 60 }));
await api(`/resources/${rid}/availability`, "POST", { windows }, owner);
// a couple of pre-existing bookings so "My bookings" isn't empty.
for (const [day, start] of [["2026-07-27", 13 * 60], ["2026-07-29", 15 * 60]])
  await api("/bookings", "POST", { resource: rid, day, start, end: start + 30 }, owner);

// ---- drive the SPA on a phone viewport --------------------------------------
const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: 414, height: 896 },
  recordVideo: { dir: OUT, size: { width: 414, height: 896 } },
  deviceScaleFactor: 2, isMobile: true, hasTouch: true, acceptDownloads: true,
});
const page = await ctx.newPage();
await page.goto(BASE);

try {
  await page.getByPlaceholder("email").fill("owner@acme.io");
  await page.getByPlaceholder("password").fill("pw12345678");
  await page.getByRole("button", { name: "Log in" }).click();
  await page.getByRole("tab", { name: "Book", exact: true }).waitFor({ timeout: 10000 });
  await sleep(900);

  // Book: pick a Monday, see the free slots, book one.
  await page.locator("input[type=date]").fill("2026-07-27");
  await sleep(1600);
  const slots = page.locator("button").filter({ hasText: /\d\d:\d\d–\d\d:\d\d/ });
  await slots.nth(2).click(); // ~10:00
  await sleep(2400); // confirmation card (rendered email + .ics)

  // weekly-repeat: book the same kind of slot for several weeks at once.
  await page.locator("input[type=checkbox]").first().check();
  await sleep(900);
  await slots.nth(4).click(); // ~11:00, repeated weekly
  await sleep(2600);

  // My bookings: the list, with .ics download + cancel.
  await page.getByRole("tab", { name: "My bookings" }).click();
  await sleep(2400);
  await page.mouse.wheel(0, 300); await sleep(1600);

  // Manage: the weekly availability grid.
  await page.getByRole("tab", { name: "Manage" }).click();
  await sleep(1400);
  await page.mouse.wheel(0, 400); await sleep(2200);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
