// Screencast: TWO editors on one document, side by side, editing the REAL
// running scribe app. Each pane is a distinct replica; an edit in one is merged
// server-side (crdt:merge lwwmap) and pushed to the other over SSE. The headline
// shot: both panes type at once into DIFFERENT fields and both edits survive —
// concurrent editing, no lock. Nothing here is faked; it drives the live SPA.
//
// Prereq: from repo root  `just host-scribe &`   (serves on :3037)
import { chromium } from "playwright";

const BASE = process.env.SCRIBE_URL || "http://127.0.0.1:3037";
const DOC = process.env.SCRIBE_DOC || "launch";
const OUT = new URL("./videos/scribe/", import.meta.url).pathname;
// wide enough that each pane clears the 760px breakpoint and shows its History
// (diff:text) rail alongside the document.
const W = 1720, H = 840;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
  deviceScaleFactor: 2,
});
const page = await ctx.newPage();

const url = (name, rid) =>
  `${BASE}/?doc=${DOC}&name=${encodeURIComponent(name)}&rid=${rid}`;
const pane = (id, label, src) =>
  `<div style="flex:1;display:flex;flex-direction:column;gap:6px;min-width:0">
     <div style="color:#8b95a7;font:600 12px system-ui;padding-left:4px">${label}</div>
     <iframe id="${id}" src="${src}" title="${label}"
       style="flex:1;border:1px solid #2a2f3a;border-radius:12px;background:#0f1115"></iframe>
   </div>`;

await page.goto(BASE);
await page.setContent(
  `<div style="display:flex;gap:14px;padding:14px;height:100vh;box-sizing:border-box;background:#05070b">
     ${pane("a", "Alice", url("Alice", "alice"))}${pane("b", "Bob", url("Bob", "bob"))}
   </div>`,
);

const A = () => page.frameLocator("#a");
const B = () => page.frameLocator("#b");
const type = (frame, sel, text) => frame().locator(sel).pressSequentially(text, { delay: 55 });

try {
  // both editors connect (wait for the "live" pill in each)
  await A().locator("#status").filter({ hasText: "live" }).waitFor({ timeout: 10000 });
  await B().locator("#status").filter({ hasText: "live" }).waitFor({ timeout: 10000 });
  await sleep(1000);

  // caret helpers (set position deterministically; keyboard Home/End is
  // platform-dependent in textareas) + blur so the pane resyncs to converged.
  const caret = (frame, at) =>
    frame().locator("#body").evaluate((el, at) => {
      el.focus();
      const p = at === "end" ? el.value.length : at;
      el.setSelectionRange(p, p);
    }, at);
  const blur = (frame) => frame().locator("#body").evaluate((el) => el.blur());

  // 1. Alice titles the doc -> Bob's title fills in live over SSE
  await A().locator("#title").click();
  await type(A, "#title", "Launch plan");
  await A().locator("#title").evaluate((el) => el.blur());
  await sleep(1400);

  // 2. Bob writes the body -> Alice sees it stream in live.
  await caret(B, 0);
  await type(B, "#body", "Ship the CRDT showcase this week.");
  await blur(B);
  await sleep(1600);

  // 3. THE RGA WIN: Alice edits the SAME body field — appends a line. Under
  // last-writer-wins this would clobber Bob's text; the RGA merges them.
  await caret(A, "end");
  await type(A, "#body", "\nOwner: Alice · Reviewer: Bob.");
  await blur(A);
  await sleep(1600);

  // 4. Bob edits the same body again — prepends at the start, Alice's line stands.
  await caret(B, 0);
  await type(B, "#body", "DRAFT — ");
  await blur(B);
  await sleep(2200);

  // hold on the converged doc — both panes show the identical merged body,
  // and both History rails show the per-revision diffs.
  await sleep(1800);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
