// Screencast: presigned direct-upload. Pick a file and watch the backend ask
// for a ticket (the policy answer, no bytes), mint a signed ticket, PUT the
// bytes straight to storage, then expose a signed download link — while a
// blocked type is refused at ticket time. The presigned control/data split in
// a real browser.
import { chromium } from "playwright";

const BASE = process.env.DROP_URL || "http://127.0.0.1:3021";
const OUT = new URL("./videos/drop/", import.meta.url).pathname;
const W = 900, H = 720;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
});
const page = await ctx.newPage();
await page.goto(BASE);

// helper: drop a synthetic File onto the hidden input.
const putFile = async (name, type, content) => {
  await page.setInputFiles("#file", {
    name,
    mimeType: type,
    buffer: Buffer.from(content),
  });
};

try {
  await sleep(900);

  // 1. an allowed text file: ticket -> upload -> signed link.
  await putFile("notes.txt", "text/plain", "hello, presigned world — the bytes never touch the control path");
  await sleep(2200);

  // 2. a second allowed file (png-ish bytes).
  await putFile("pixel.png", "image/png", "\x89PNG\r\n\x1a\n fake but allowed");
  await sleep(2000);

  // 3. a blocked type — refused at TICKET time, no bytes uploaded.
  await putFile("evil.bin", "application/x-evil", "should never be stored");
  await sleep(2200);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
