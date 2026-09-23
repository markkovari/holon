// photoquest — register, upload a 129 MB Sony ARW straight to the store, watch it
// come back evaluated on-device (ADR-0098).
//
// Needs the app running with its media daemon, the store and a JetStream NATS —
// see docs/apps/PHOTOQUEST.md. From tools/screencast:
//
//   node photoquest.mjs [path/to/file.ARW]   (default: the CC0 sample below)
//   bash to-gif.sh videos/photoquest/*.webm ../../docs/media/photoquest.gif 800 10 1
//
// Warm the pipeline with one upload first: the first job after a start pays Core
// Image's cold start (~4 s), every later one ~2 s. Nothing here is sped up.
import { chromium } from "playwright";
import { fileURLToPath } from "url";
import { dirname, join } from "path";
import { homedir } from "os";

const __dirname = dirname(fileURLToPath(import.meta.url));
const OUT = join(__dirname, "videos/photoquest/");
const BASE = "http://127.0.0.1:3941";
// Default: a CC0 sample from raw.pixls.us (a7R V, lossless uncompressed) — never a
// personal photo; a recording ends up in a public repository.
//   curl -fLO https://raw.pixls.us/data/Sony/ILCE-7RM5/7RM5-LosslessUncompressed.ARW
const FILE = process.argv[2] || join(homedir(), "Downloads/photoquest-samples/7RM5-LosslessUncompressed.ARW");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: 900, height: 1000 },
  recordVideo: { dir: OUT, size: { width: 900, height: 1000 } },
  deviceScaleFactor: 1,
});
const page = await ctx.newPage();
await page.goto(BASE);
await sleep(900);

await page.locator("#reg-email").pressSequentially(`photographer${Date.now() % 1000}@example.com`, { delay: 35 });
await page.locator("#reg-password").pressSequentially("correct horse battery", { delay: 25 });
await sleep(300);
await page.click("#register-btn");
await page.waitForSelector("#app", { state: "visible", timeout: 15000 });
await sleep(900);

await page.setInputFiles("#file", FILE);
await sleep(700);
await page.click("#upload-btn");
await page.waitForFunction(
  () => /evaluated|failed/.test(document.getElementById("upload-status").textContent),
  null,
  { timeout: 120000 },
);
await page.waitForFunction(
  () => [...document.querySelectorAll("img")].every((i) => i.complete && i.naturalWidth > 0),
  null,
  { timeout: 30000 },
);
await sleep(1200);

// Walk down through the evaluation, then hold on it.
const detail = page.locator("#detail");
const box = await detail.boundingBox();
for (let y = 0; y <= (box?.y ?? 0) + (box?.height ?? 0) - 1000 + 40; y += 40) {
  await page.evaluate((top) => window.scrollTo(0, top), y);
  await sleep(30);
}
await sleep(3500);

await ctx.close();
await browser.close();
