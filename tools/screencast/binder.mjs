// Screencast: the card binder SPA, on the REAL running app.
//
// Seeds a collection through the app's own HTTP surface — so everything on screen
// travelled through the composition: `card:identify` reads a vision model's answer,
// `price:history` decides what a card was worth on the days between quotes,
// `portfolio:value` does the FIFO arithmetic, and `deck:build` decides whether a
// deck is legal. Nothing here computes a number the page then displays.
//
// The seed is chosen to make the recording say something true rather than pretty:
//
//   * a fenced model answer, the way a model actually replies;
//   * an INCOMPLETE one, so rows show fields marked `check` — the AI guessed and
//     nobody has confirmed them;
//   * bulk commons nothing will ever quote, so "unpriced" is on screen next to the
//     total it is deliberately not folded into;
//   * buy 2 @ 10.00, buy 1 @ 40.00, sell 1 @ 30.00 — FIFO realises 20.00;
//   * a deck with EIGHT Charmander across two printings, which is illegal by a rule
//     that counts names rather than the ids the collection is keyed on.
//
// Prereq: from repo root  `just host-binder &`   (serves on :3210)
import { chromium } from "playwright";

const BASE = process.env.BINDER_URL || "http://127.0.0.1:3210";
const OUT = new URL("./videos/binder/", import.meta.url).pathname;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const DAY = 86400;
const now = Math.floor(Date.now() / 1000);

let token = "";
const call = (path, method, body) =>
  fetch(BASE + path, {
    method,
    headers: { "content-type": "application/json", ...(token ? { authorization: `Bearer ${token}` } : {}) },
    body: body === undefined ? undefined : JSON.stringify(body),
  }).then((r) => r.json().catch(() => null));

// --- seed, through the app's own routes ------------------------------------

const EMAIL = `demo${now}@binder.test`;
const PASSWORD = "pw12345678";
await call("/api/register", "POST", { email: EMAIL, password: PASSWORD });
token = (await call("/api/login", "POST", { email: EMAIL, password: PASSWORD })).access_token;

const fenced = `Looking at the photo:
\`\`\`json
{"name":"Charizard ex","set_name":"Obsidian Flames","set_code":"SV3","number":"125/197",
 "rarity":"Double Rare","language":"en","variant":"holo","condition":"near mint","confidence":88}
\`\`\`
Hope that helps.`;

const charizard = await call("/api/scan", "POST", { answer: fenced });
// `at` on purpose: without it an acquisition defaults to NOW, and a collection
// bought over a year appears in one instant at the right edge of the chart. The
// number would be right and the shape would be a lie.
const charmander = await call("/api/cards", "POST", {
  name: "Charmander", set_name: "Obsidian Flames", set_code: "sv3", number: "026/197",
  printing: "normal", condition: "near mint", paid_minor: 120, quantity: 4,
  at: now - 250 * DAY,
});
const oldCharmander = await call("/api/cards", "POST", {
  name: "Charmander", set_name: "Base", set_code: "base1", number: "046/102",
  printing: "shadowless", condition: "lightly played", paid_minor: 4500, quantity: 4,
  at: now - 140 * DAY,
});
// An answer the model could not finish: those fields show as `check`.
const pikachu = await call("/api/scan", "POST", {
  answer: '{"name":"Pikachu","set_name":"Base","set_code":"base1","number":"58/102","confidence":41}',
});
const energy = await call("/api/cards", "POST", {
  name: "Fire Energy", set_name: "Scarlet & Violet Energy", set_code: "sve", number: "002",
  printing: "normal", condition: "near mint", paid_minor: 5, quantity: 47,
  at: now - 210 * DAY,
});

// buy 2 @ 10.00, buy 1 @ 40.00, sell 1 @ 30.00 — FIFO realises 20.00, not 10.00
for (const [kind, quantity, unit_minor, ago] of [
  ["acquired", 2, 1000, 300],
  ["acquired", 1, 4000, 180],
  ["disposed", 1, 3000, 90],
]) {
  await call("/api/events", "POST", { card_id: charizard.id, kind, quantity, unit_minor, currency: "EUR", at: now - ago * DAY });
}
// Bulk nobody prices.
await call("/api/events", "POST", { card_id: pikachu.id, kind: "acquired", quantity: 40, unit_minor: 5, at: now - 260 * DAY });

// Sparse quotes with real gaps, which is what makes the stepped, carried-forward
// segments visible rather than a smooth invented curve.
for (const [ago, unit_minor] of [
  [290, 3800], [240, 4200], [200, 4500], [150, 5100], [120, 6000],
  [80, 7200], [45, 8800], [20, 9000], [5, 8600],
]) {
  await call("/api/quotes", "POST", { card_id: charizard.id, unit_minor, currency: "EUR", at: now - ago * DAY });
}

// A deck that is illegal for the reason worth showing: 8 Charmander across TWO
// printings, which the id-keyed collection reads as four and four.
await call("/api/decks", "POST", { name: "charizard ex" });
for (const [card_id, quantity, kind] of [
  [charmander.id, 4, "basic-pokemon"],
  [oldCharmander.id, 4, "basic-pokemon"],
  [charizard.id, 4, "evolved-pokemon"],
  [energy.id, 47, "basic-energy"],
]) {
  await call("/api/decks/charizard%20ex/slots", "POST", { card_id, quantity, kind });
}

// --- a card to photograph ---------------------------------------------------
//
// Drawn here and screenshotted, rather than shipping a scan of a real card: the art
// on a Pokémon card is somebody's, and a repository does not need a copy of it to
// demonstrate that a model can read one. The fields the app cares about — name, set,
// number, HP — are exactly what a photograph would show, which is the whole of what
// `card:identify` reads.
const CARD_MOCK = `<!doctype html><meta charset="utf-8"><style>
  body { margin: 0; }
  .card { width: 340px; height: 476px; border-radius: 16px; padding: 12px;
          background: linear-gradient(160deg, #f7d84a, #e8b923); font: 14px/1.3 Helvetica, Arial, sans-serif;
          box-sizing: border-box; color: #221c07; }
  .inner { background: #fffdf2; border-radius: 8px; height: 100%; padding: 10px; box-sizing: border-box;
           display: flex; flex-direction: column; border: 2px solid #cfa60d; }
  .top { display: flex; align-items: baseline; }
  .name { font-size: 21px; font-weight: 800; }
  .hp { margin-left: auto; font-size: 17px; font-weight: 800; color: #b0161b; }
  .art { flex: 1; margin: 8px 0; border: 3px solid #cfa60d; border-radius: 4px;
         background: radial-gradient(circle at 50% 42%, #ffe98a, #f2c53d 62%, #d9a520);
         display: grid; place-items: center; font-size: 74px; }
  .box { border: 1px solid #d9c680; border-radius: 4px; padding: 7px; background: #fffbe8; }
  .move { display: flex; align-items: baseline; font-weight: 700; }
  .move .dmg { margin-left: auto; font-size: 19px; }
  .txt { font-weight: 400; font-size: 11.5px; color: #4a412a; margin-top: 3px; }
  .foot { display: flex; font-size: 11px; margin-top: 8px; color: #5b5232; }
  .foot .no { margin-left: auto; font-weight: 700; }
</style><div class="card"><div class="inner">
  <div class="top"><span class="name">Pikachu</span><span class="hp">60 HP</span></div>
  <div class="art">⚡</div>
  <div class="box"><div class="move">Thunder Jolt <span class="dmg">30</span></div>
    <div class="txt">Flip a coin. If tails, Pikachu does 10 damage to itself.</div></div>
  <div class="foot"><span>Base Set · Common</span><span class="no">058/102</span></div>
</div></div>`;

// --- record -----------------------------------------------------------------

const browser = await chromium.launch({ headless: true });
// A throwaway context for the card mock, so the recording only holds the app.
const ctx0 = await browser.newContext();
const shotPage = await ctx0.newPage();
await shotPage.setViewportSize({ width: 340, height: 476 });
await shotPage.setContent(CARD_MOCK);
const CARD_PNG = await shotPage.screenshot({ type: "png" });
await shotPage.close();

const ctx = await browser.newContext({
  viewport: { width: 1000, height: 720 },
  recordVideo: { dir: OUT, size: { width: 1000, height: 720 } },
  deviceScaleFactor: 1,
});
const page = await ctx.newPage();

try {
  // The SPA keeps its session in localStorage, so the recording starts signed in
  // rather than spending four seconds typing a password. Set AFTER the first load —
  // localStorage belongs to an origin, so there is nothing to write to until the
  // page has been there once.
  await page.goto(BASE);
  await page.evaluate((t) => localStorage.setItem("binder-tok", t), token);
  await page.reload();
  // The tile, not the filter chip that carries the same words.
  await page.locator("div.text-xs", { hasText: /^market value$/ }).first().waitFor({ timeout: 15000 });
  await sleep(1800);

  // Hover the chart: every point carries the numbers the valuation computed.
  const chart = page.locator("svg.recharts-surface").first();
  const box = await chart.boundingBox();
  for (const f of [0.25, 0.45, 0.62, 0.8]) {
    await page.mouse.move(box.x + box.width * f, box.y + box.height * 0.45);
    await sleep(900);
  }

  // The time range is a server query, not a crop.
  for (const label of ["30d", "1y", "All"]) {
    await page.getByRole("button", { name: label, exact: true }).click();
    await sleep(1300);
  }
  // And the series can be turned on and off.
  await page.getByRole("button", { name: "realised", exact: true }).click();
  await sleep(1200);

  await page.getByRole("link", { name: "Cards" }).click();
  await sleep(1600);

  // The photograph. The upload answers immediately with a job and the stream says
  // what is happening while the model looks — `looking`, then `reading`, then the
  // card. That is the part worth seeing: nothing here is a spinner.
  await page.setInputFiles('input[type="file"]', {
    name: "pikachu.png", mimeType: "image/png", buffer: CARD_PNG,
  });
  // Long enough for a real vision call to finish and the row to appear.
  await sleep(22000);
  await page.mouse.wheel(0, 340);
  await sleep(2200);

  await page.getByRole("link", { name: "Decks" }).click();
  await sleep(1400);
  await page.getByText("charizard ex").first().click();
  await sleep(2600);
  await page.mouse.wheel(0, 320);
  await sleep(2000);
} finally {
  await ctx.close();
  await browser.close();
}
console.error(`recorded to ${OUT}`);
