// Screencast: the buzz live quiz game (React + shadcn UI) on the REAL running
// app. Records the HOST big-screen — the lobby fills as players JOIN (driven via
// the API in the background), then a question shows the four colored options with
// a live answered-count, Reveal highlights the correct one + a leaderboard, and
// on it goes to a final podium.
//
// Prereq: from repo root  `just host-buzz &`   (builds the UI, serves on :3049)
import { chromium } from "playwright";

const BASE = process.env.BUZZ_URL || "http://127.0.0.1:3049";
const OUT = new URL("./videos/buzz/", import.meta.url).pathname;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function api(path, body) {
  const r = await fetch(`${BASE}/api${path}`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  return r.json().catch(() => ({}));
}

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: 820, height: 820 },
  recordVideo: { dir: OUT, size: { width: 820, height: 820 } },
  deviceScaleFactor: 1,
});
const page = await ctx.newPage();
await page.goto(BASE);

// players join + answer through the API so they appear live on the host screen.
let players = {};
async function join(nick) { players[nick] = (await api("/games/" + PIN + "/join", { nickname: nick })).player; }
let PIN = "";
async function answer(nick, option) { await api(`/games/${PIN}/answer`, { player: players[nick], option }); }

try {
  // host signs in and starts hosting the demo quiz.
  await page.getByText("Host a game →").click(); await sleep(300);
  await page.getByRole("button", { name: "Register" }).click();
  await page.getByText("Host a game").first().waitFor({ timeout: 10000 });
  await sleep(500);
  await page.getByRole("button", { name: "Host" }).first().click();
  await sleep(900);
  PIN = (await page.locator(".text-7xl").innerText()).trim();

  // players trickle into the lobby.
  await join("Ada"); await sleep(700);
  await join("Bo"); await sleep(700);
  await join("Cy"); await sleep(1400);

  // Q1
  await page.getByRole("button", { name: "Start game" }).click();
  await sleep(1400);
  await answer("Ada", 1); await sleep(600);         // correct, fast
  await answer("Bo", 1); await sleep(500);          // correct, slower
  await answer("Cy", 0); await sleep(1200);         // wrong
  await page.getByRole("button", { name: "Reveal" }).click();
  await sleep(2600); // correct highlighted + leaderboard

  // Q2
  await page.getByRole("button", { name: "Next" }).click();
  await sleep(1300);
  await answer("Bo", 0); await sleep(400);
  await answer("Ada", 0); await sleep(400);
  await answer("Cy", 2); await sleep(900);
  await page.getByRole("button", { name: "Reveal" }).click();
  await sleep(2400);

  // Q3 -> final podium
  await page.getByRole("button", { name: "Next" }).click();
  await sleep(1100);
  await answer("Ada", 1); await answer("Bo", 1); await answer("Cy", 1); await sleep(900);
  await page.getByRole("button", { name: "Reveal" }).click();
  await sleep(1400);
  await page.getByRole("button", { name: "Next" }).click();
  await sleep(2600); // final podium
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
