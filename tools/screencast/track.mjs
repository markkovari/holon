// Screencast: the track project board — the complex showcase. Register as admin,
// create a project, file a couple of issues, move one across the board, open it
// to comment + AI-summarize, then search — while the live SSE activity feed
// fills on the right. Five axes in one composed wasm with a baked SPA.
import { chromium } from "playwright";

const BASE = process.env.TRACK_URL || "http://127.0.0.1:3025";
const OUT = new URL("./videos/track/", import.meta.url).pathname;
const W = 1180, H = 800;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
});
const page = await ctx.newPage();

// auto-accept the prompt() dialogs the SPA uses for create flows.
const answers = [];
page.on("dialog", async (d) => {
  try {
    const a = answers.shift();
    if (a === undefined) await d.dismiss();
    else await d.accept(a);
  } catch { /* dialog already handled / page closing */ }
});

await page.goto(BASE);

try {
  await sleep(800);
  // register (first user = admin) — the form is prefilled admin@track.io.
  await page.click("#register");
  await sleep(1500);

  // create a project: key ENG, name Engineering.
  answers.push("ENG", "Engineering");
  await page.click("#newProj");
  await sleep(1500);

  // file two issues.
  answers.push("Login token expires too early", "The expiry check uses `<` not `<=` — off-by-one at the boundary.", "bug");
  await page.click("#newIssue");
  await sleep(1400);
  answers.push("Dark mode flickers on load", "Theme applied after first paint; flash of light theme.", "ui");
  await page.click("#newIssue");
  await sleep(1400);

  // open the first issue, move it start -> begin, comment, summarize.
  await page.click(".card");
  await sleep(1200);
  await page.click('[data-move="start"]'); // backlog -> todo (closes modal, reloads)
  await sleep(1200);
  await page.click(".card"); // reopen (now in todo)
  await sleep(900);
  await page.click('[data-move="begin"]'); // todo -> in progress
  await sleep(1200);
  await page.click(".card");
  await sleep(800);
  const cmt = await page.$("#cmt");
  if (cmt) { await page.fill("#cmt", "Patch up — clamps the comparison. Ready for review."); await page.click("#reply"); await sleep(1200); }
  const ai = await page.$("#ai");
  if (ai) { await page.click("#ai"); await sleep(2000); } // AI summary appears
  const close = await page.$("#close");
  if (close) { await page.click("#close"); await sleep(600); }

  // search narrows the board.
  await page.fill("#q", "token");
  await page.click("#search");
  await sleep(2200);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
