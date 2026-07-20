// Screencast: pulse realtime chat. Two panes (Ada + Bob) side by side in one
// page; a message typed in one appears in the other LIVE over SSE — proving the
// held-open server-push stream end to end in a real browser.
import { chromium } from "playwright";

const BASE = process.env.PULSE_URL || "http://127.0.0.1:3015";
const OUT = new URL("./videos/pulse/", import.meta.url).pathname;
const W = 1300, H = 720;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
});
const page = await ctx.newPage();

const pane = (id, name) =>
  `<iframe id="${id}" src="${BASE}/?name=${name}&room=demo" title="${name}"
     style="flex:1;border:1px solid #2a2f3a;border-radius:12px;background:#0f1115"></iframe>`;
await page.setContent(
  `<div style="display:flex;gap:14px;padding:14px;height:100vh;box-sizing:border-box;background:#05070b">
     ${pane("a", "Ada")}${pane("b", "Bob")}
   </div>`
);

const say = async (frame, text) => {
  const f = page.frameLocator(frame);
  await f.locator("#text").click();
  await f.locator("#text").fill(text);
  await f.locator("#text").press("Enter");
  await sleep(1700);
};

try {
  // wait for both panes' SSE to go live
  await page.frameLocator("#a").locator("#status").filter({ hasText: "live" }).waitFor({ timeout: 10000 });
  await page.frameLocator("#b").locator("#status").filter({ hasText: "live" }).waitFor({ timeout: 10000 });
  await sleep(1200);

  await say("#a", "Hey Bob 👋");
  await say("#b", "Whoa — this is live over SSE");
  await say("#a", "one wasm component, no WebSocket");
  await say("#b", "record-store + event-bus, composed 🎉");
  await say("#a", "and it survives on the native Rust host");
  await sleep(2500);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
