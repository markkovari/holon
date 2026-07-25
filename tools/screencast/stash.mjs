// Screencast: the stash note app (React + shadcn UI) on the REAL running app.
// Registers (seeding demo notes), opens a note and edits it, adds a new note,
// then hits Export .zip — the header button downloads stash-export.zip, built by
// the composed zip:archive component (no zip library in the frontend).
//
// Prereq: from repo root  `just host-stash &`   (builds the UI, serves on :3046)
import { chromium } from "playwright";

const BASE = process.env.STASH_URL || "http://127.0.0.1:3046";
const OUT = new URL("./videos/stash/", import.meta.url).pathname;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: 900, height: 680 },
  recordVideo: { dir: OUT, size: { width: 900, height: 680 } },
  deviceScaleFactor: 1, acceptDownloads: true,
});
const page = await ctx.newPage();
await page.goto(BASE);

try {
  await page.getByPlaceholder("email").fill("you@acme.io");
  await page.getByPlaceholder("password").fill("pw12345678");
  await page.getByRole("button", { name: "Register" }).click();
  await page.getByText(/Notes \(/).waitFor({ timeout: 10000 });
  await sleep(1000);

  // browse the seeded notes
  await page.getByRole("button", { name: /Idea/ }).click(); await sleep(1400);
  await page.getByRole("button", { name: /Shopping list/ }).click(); await sleep(800);
  // edit + save
  await page.locator("textarea").fill("- coffee\n- oat milk\n- a WIT component or two\n- export me!");
  await page.getByRole("button", { name: "Save" }).click(); await sleep(1200);

  // export the zip (downloads stash-export.zip)
  const [dl] = await Promise.all([
    page.waitForEvent("download"),
    page.getByRole("button", { name: "Export .zip" }).click(),
  ]);
  await dl.path();
  await sleep(2200);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
