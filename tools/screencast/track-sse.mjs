// Screencast: the track SSE activity feed across TWO panes. Left pane files /
// moves issues; the RIGHT pane — a separate board instance — sees each change
// land in its activity feed LIVE over SSE and its board reload, proving the
// event-bus fan-out end to end. One wasm component, no WebSocket.
import { chromium } from "playwright";

const BASE = process.env.TRACK_URL || "http://127.0.0.1:3025";
const OUT = new URL("./videos/track-sse/", import.meta.url).pathname;
const W = 1320, H = 780;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Seed an admin + a project over the API, so both panes boot straight onto the
// board (the SPA reads the token from localStorage, shared across same-origin
// iframes). Returns {token, project}.
async function seed() {
  const post = async (path, body) => {
    const r = await fetch(BASE + path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    return { ok: r.ok, d: await r.json().catch(() => ({})) };
  };
  await post("/auth/register", { email: "demo@track.io", password: "pw12345678", role: "admin" });
  const login = await post("/auth/login", { email: "demo@track.io", password: "pw12345678" });
  const token = login.d.access_token;
  const proj = await fetch(BASE + "/api/projects", { method: "POST", headers: { "content-type": "application/json", authorization: `Bearer ${token}` }, body: JSON.stringify({ key: "ENG", name: "Engineering" }) });
  const project = (await proj.json()).id;
  return { token, project };
}

const { token, project } = await seed();

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({ viewport: { width: W, height: H }, recordVideo: { dir: OUT, size: { width: W, height: H } } });
const page = await ctx.newPage();

// prime localStorage so the SPA in each iframe boots logged-in.
await page.goto(BASE);
await page.evaluate((t) => localStorage.setItem("track_tok", t), token);

const pane = (id, label) =>
  `<div style="flex:1;display:flex;flex-direction:column;gap:6px">
     <div style="color:#8b95a7;font:600 12px system-ui;padding-left:4px">${label}</div>
     <iframe id="${id}" src="${BASE}/" title="${label}"
       style="flex:1;border:1px solid #2a2f3a;border-radius:12px;background:#0f1115"></iframe>
   </div>`;
await page.setContent(
  `<div style="display:flex;gap:14px;padding:14px;height:100vh;box-sizing:border-box;background:#05070b">
     ${pane("a", "Alice — files & moves issues")}${pane("b", "Bob — watches the live feed")}
   </div>`,
);

const A = () => page.frameLocator("#a");
const B = () => page.frameLocator("#b");

// prompt() answers for the LEFT pane's create flows.
const answers = [];
page.on("dialog", async (d) => { try { const a = answers.shift(); a === undefined ? await d.dismiss() : await d.accept(a); } catch {} });

try {
  // both boards live; wait for the SSE "live" badge in each.
  await A().locator("#live").filter({ hasText: "live" }).waitFor({ timeout: 10000 });
  await B().locator("#live").filter({ hasText: "live" }).waitFor({ timeout: 10000 });
  await sleep(1200);

  // LEFT files an issue -> RIGHT's feed shows issue.created, board reloads.
  answers.push("Login token expires too early", "Off-by-one in the expiry check.", "bug");
  await A().locator("#newIssue").click();
  await sleep(2200);

  // LEFT files a second -> RIGHT sees it too.
  answers.push("Dark mode flickers", "Flash of light theme on first paint.", "ui");
  await A().locator("#newIssue").click();
  await sleep(2200);

  // LEFT opens the first card and moves it -> RIGHT sees issue.moved live.
  await A().locator(".card").first().click();
  await sleep(900);
  await A().locator('[data-move="start"]').click();
  await sleep(2400);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
