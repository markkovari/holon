// Screencast: the card binder, on the REAL running app.
//
// Seeds a collection through the app's own HTTP surface — so everything the video
// shows has travelled through the composition: `card:identify` reads a vision
// model's answer, `price:history` decides what a card was worth on the days between
// quotes, and `portfolio:value` does the FIFO arithmetic. Nothing here computes a
// number the page then displays.
//
// The seed is chosen to make the recording say something true rather than pretty:
//
//   * a fenced model answer, the way a model actually replies;
//   * an INCOMPLETE answer, so the page shows fields marked `check` — the AI guessed
//     and nobody has confirmed them, which is the whole point of `needs_review`;
//   * forty bulk commons nothing will ever quote, so the "unpriced" count is on
//     screen next to the total it is deliberately not folded into;
//   * buy 2 @ €10.00, buy 1 @ €40.00, sell 1 @ €30.00 — FIFO realises €20.00.
//
// Prereq: from repo root  `just host-binder &`   (serves on :3210)
import { chromium } from "playwright";

const BASE = process.env.BINDER_URL || "http://127.0.0.1:3210";
const OUT = new URL("./videos/binder/", import.meta.url).pathname;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const DAY = 86400;
const now = Math.floor(Date.now() / 1000);

const post = (path, body) =>
  fetch(BASE + path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  }).then((r) => r.json());

// --- seed, through the app's own routes ------------------------------------

const fenced = `Looking at the photo:
\`\`\`json
{"name":"Charizard ex","set_name":"Obsidian Flames","set_code":"SV3","number":"125/197",
 "rarity":"Double Rare","language":"en","variant":"holo","condition":"near mint","confidence":88}
\`\`\`
Hope that helps.`;

const charizard = await post("/api/scan", { answer: fenced });
const pikachu = await post("/api/scan", {
  answer: '{"name":"Pikachu","set_name":"Base","set_code":"base1","number":"58/102","confidence":41}',
});
const misprint = await post("/api/scan", {
  answer: '{"name":"Mew","set_name":"Wizards Black Star","set_code":"wbsp","confidence":22,"uncertain":["number"]}',
});

for (const [kind, quantity, unit_minor, ago] of [
  ["acquired", 2, 1000, 60],
  ["acquired", 1, 4000, 40],
  ["disposed", 1, 3000, 20],
]) {
  await post("/api/events", { card_id: charizard.id, kind, quantity, unit_minor, currency: "EUR", at: now - ago * DAY });
}
// The bulk nobody prices, and one card that has never traded.
await post("/api/events", { card_id: pikachu.id, kind: "acquired", quantity: 40, unit_minor: 5, at: now - 50 * DAY });
await post("/api/events", { card_id: misprint.id, kind: "acquired", quantity: 1, unit_minor: 1200, at: now - 35 * DAY });

// Sparse quotes, with real gaps between them — which is what makes the chart's
// carried-forward segments visible rather than a smooth invented curve.
for (const [ago, unit_minor] of [[58, 3800], [45, 4500], [30, 6000], [18, 7200], [10, 9000], [2, 8600]]) {
  await post("/api/quotes", { card_id: charizard.id, unit_minor, currency: "EUR", at: now - ago * DAY });
}

// --- record -----------------------------------------------------------------

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: 900, height: 680 },
  recordVideo: { dir: OUT, size: { width: 900, height: 680 } },
  deviceScaleFactor: 1,
});
const page = await ctx.newPage();

try {
  await page.goto(BASE);
  await page.getByText("market value").waitFor({ timeout: 10000 });
  await sleep(2200);

  // The totals, then the chart, then the rows and their `check` flags.
  await page.mouse.wheel(0, 120);
  await sleep(1600);
  await page.mouse.wheel(0, 160);
  await sleep(2400);
  await page.mouse.wheel(0, -280);
  await sleep(1500);
} finally {
  await ctx.close();
  await browser.close();
}
console.error(`recorded to ${OUT}`);
