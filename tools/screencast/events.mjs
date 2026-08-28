// Screencast: free event ticketing, both users at once, on the REAL running app.
//
// The two panes are iframes of the same SPA under `?as=attendee` and
// `?as=organizer`, served from the app's own static dir — so every frame on both
// sides is the composed component answering, and the two are looking at one store.
//
// What the recording is meant to say, in order:
//
//   * the attendee claims a place, and the organizer's count moves — the number on
//     the right is `quota:meter`'s, read back through GET /api/events/{id}, not a
//     tally the page kept;
//   * the QR is `qr:encode`'s SVG of a `nanoid` that possession of IS the claim;
//   * the door admits it once. Scanned again it is REFUSED, carrying the state
//     `fsm:workflow` reports — which is the whole reason the lifecycle is a
//     definition rather than an `if` in the handler.
//
// Prereq: from repo root  `just host-events &`   (serves on :3230)
import { chromium } from "playwright";

const BASE = process.env.EVENTS_URL || "http://127.0.0.1:3230";
const OUT = new URL("./videos/events/", import.meta.url).pathname;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch();
const ctx = await browser.newContext({
  viewport: { width: 1280, height: 620 },
  recordVideo: { dir: OUT, size: { width: 1280, height: 620 } },
  deviceScaleFactor: 2,
});
const page = await ctx.newPage();
await page.goto(`${BASE}/split.html`);

const attendee = page.frameLocator("#attendee");
const organizer = page.frameLocator("#organizer");

// Both panes sign in through the fixture and render.
await attendee.locator("text=Open events").waitFor({ timeout: 20000 });
await organizer.locator("text=Scan a ticket").waitFor({ timeout: 20000 });
await sleep(1200);

// --- the attendee takes a place -------------------------------------------
await attendee.locator("button:has-text('Claim')").first().click();
await attendee.locator("text=ticket issued").waitFor({ timeout: 15000 });
await sleep(1600);

// The organizer's count is the meter's, so it only moves after a reload.
await page.evaluate(() => document.getElementById("organizer").contentWindow.location.reload());
await organizer.locator("text=Scan a ticket").waitFor({ timeout: 20000 });
await sleep(1600);

// --- the code travels from one screen to the other ------------------------
const code = (await attendee.locator("div.font-mono").first().innerText()).trim();
await sleep(600);

const scan = organizer.locator("input[placeholder='paste or scan the code']");
await scan.click();
await scan.type(code, { delay: 45 });
await sleep(700);
await organizer.locator("button:has-text('Check in')").click();
await organizer.locator("text=admitted").waitFor({ timeout: 15000 });
await sleep(1800);

// The attendee's own ticket now reads checked-in — one store, two views.
await page.evaluate(() => document.getElementById("attendee").contentWindow.location.reload());
await attendee.locator("text=checked-in").waitFor({ timeout: 20000 });
await sleep(1800);

// --- and the same code will not get anybody in twice ----------------------
await scan.click();
await scan.type(code, { delay: 30 });
await sleep(500);
await organizer.locator("button:has-text('Check in')").click();
await organizer.locator("text=already_checked_in").waitFor({ timeout: 15000 });
await sleep(2400);

await ctx.close();
await browser.close();
console.log(`recorded to ${OUT}`);
