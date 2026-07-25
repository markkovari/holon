// Screencast: the dashboards app (React + shadcn UI) on the REAL running app, at
// a PHONE viewport. A new account signs in to a seeded demo dashboard whose four
// panels are rendered to SVG on the server (bar / line / donut / sparkline); then
// the Add-panel form takes a title, a kind and "label value" lines and a new
// donut appears — all with no charting library in the frontend.
//
// Prereq: from repo root  `just host-dashboards &`   (builds the UI, serves on :3043)
import { chromium } from "playwright";

const BASE = process.env.DASHBOARDS_URL || "http://127.0.0.1:3043";
const OUT = new URL("./videos/dashboards/", import.meta.url).pathname;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: 414, height: 896 },
  recordVideo: { dir: OUT, size: { width: 414, height: 896 } },
  deviceScaleFactor: 2, isMobile: true, hasTouch: true,
});
const page = await ctx.newPage();
await page.goto(BASE);

try {
  // register -> seeded demo dashboard
  await page.getByPlaceholder("email").fill("you@acme.io");
  await page.getByPlaceholder("password").fill("pw12345678");
  await page.getByRole("button", { name: "Register" }).click();
  await page.getByText("Add a panel").waitFor({ timeout: 10000 });
  await sleep(1600); // charts fetch + render

  // scroll through the four server-rendered charts
  await page.mouse.wheel(0, 500); await sleep(1800);
  await page.mouse.wheel(0, 600); await sleep(1800);

  // add a new panel: title + donut + data
  await page.locator("input[placeholder='Panel title']").fill("Pets");
  await page.locator("textarea").fill("Cats 12\nDogs 9\nBirds 4\nFish 3");
  await page.locator("button[role=combobox]").last().click(); await sleep(400);
  await page.getByRole("option", { name: "donut" }).click(); await sleep(300);
  await page.getByRole("button", { name: "Add panel" }).click();
  await sleep(1600);
  // scroll to reveal the freshly rendered panel
  await page.mouse.wheel(0, 500); await sleep(2400);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
