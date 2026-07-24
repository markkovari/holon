// Screencast: two players on one Connect Four game, side by side, on the REAL
// running app. Alice creates a game (Red), Bob joins (Yellow), and they play
// live — every move is validated server-side and streamed to both boards over
// SSE. Red stacks a column and wins; the winning line lights up in both panes.
// Nothing is faked; it drives the live SPA.
//
// Prereq: from repo root  `just host-arena &`   (serves on :3039)
import { chromium } from "playwright";

const BASE = process.env.ARENA_URL || "http://127.0.0.1:3039";
const OUT = new URL("./videos/arena/", import.meta.url).pathname;
const W = 1120, H = 700;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
  deviceScaleFactor: 2,
});
const page = await ctx.newPage();

const pane = (name, label) =>
  `<div style="flex:1;display:flex;flex-direction:column;gap:6px;min-width:0">
     <div style="color:#8b95a7;font:600 12px system-ui;padding-left:6px">${label}</div>
     <iframe name="${name}" src="${BASE}" title="${label}"
       style="flex:1;border:1px solid #2a2f3a;border-radius:12px;background:#0f1115"></iframe>
   </div>`;
await page.goto(BASE);
await page.setContent(
  `<div style="display:flex;gap:14px;padding:14px;height:100vh;box-sizing:border-box;background:#05070b">
     ${pane("a", "Alice (Red)")}${pane("b", "Bob (Yellow)")}
   </div>`,
);

const A = () => page.frameLocator('iframe[name="a"]');
const B = () => page.frameLocator('iframe[name="b"]');
const drop = (who, col) => who().locator(".board .col").nth(col).click();

try {
  await A().locator("#new").waitFor({ timeout: 10000 });

  // Alice creates the game (Red).
  await A().locator("#name").fill("Alice");
  await A().locator("#new").click();
  await sleep(1000);
  const id = await page.frame("a").evaluate(() => window.__game?.id);

  // Bob joins by id (Yellow).
  await B().locator("#gid").fill(id);
  await B().locator("#name").fill("Bob");
  await B().locator("#join").click();
  await sleep(1400);

  // Play: Red stacks column 3, Yellow stacks column 2 — alternating turns,
  // each move streamed live to both boards. Red gets four vertically and wins.
  const seq = [[A, 3], [B, 2], [A, 3], [B, 2], [A, 3], [B, 2], [A, 3]];
  for (const [who, col] of seq) {
    await drop(who, col);
    await sleep(1000);
  }

  // hold on the win (the four-in-a-row glows in both panes).
  await sleep(2600);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
