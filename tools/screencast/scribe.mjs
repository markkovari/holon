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

  // 1. Alice titles the doc -> Bob's title fills in live over SSE
  await A().locator("#title").click();
  await type(A, "#title", "Launch plan");
  await sleep(1700);

  // 2. Bob writes the body (a different field) -> Alice sees it appear live.
  // Different fields, edited from different replicas, both survive the merge.
  await B().locator("#body").click();
  await type(B, "#body", "Ship the CRDT showcase this week.");
  await sleep(1900);

  // 3. Edits keep flowing BOTH ways — no lock, no turn-taking. Alice revises the
  // title while Bob's body stands; Bob appends a line while Alice's title stands.
  await A().locator("#title").click();
  await A().locator("#title").press("ControlOrMeta+a");
  await type(A, "#title", "Launch plan — v2");
  await sleep(1500);

  await B().locator("#body").click();
  await B().locator("#body").press("End");
  await type(B, "#body", "\nOwner: Bob · Reviewer: Alice.");
  await sleep(2400);

  // hold on the converged doc — both panes show the identical merged document
  await sleep(1800);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
