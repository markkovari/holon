// Screencast: the tempo worktime logger (React + shadcn UI) on the REAL running
// app, recorded at a PHONE viewport to show it's mobile-friendly. Seeds a team
// (projects, categories, per-project memberships, time entries) via the API,
// then drives the SPA as a project lead: the Reports tab shows the team's
// distribution (donut by project + category / per-day / per-person bars via
// recharts), flips the range and the Mine/Everyone scope, then the Log tab runs
// a live pomodoro timer that logs an entry on stop.
//
// Prereq: from repo root  `just host-tempo &`   (builds the UI, serves on :3040)
import { chromium } from "playwright";

const BASE = process.env.TEMPO_URL || "http://127.0.0.1:3040";
const OUT = new URL("./videos/tempo/", import.meta.url).pathname;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---- seed via the API (membership required to log) --------------------------
const HJ = { "content-type": "application/json" };
async function api(path, method = "GET", body, token) {
  const h = { ...HJ }; if (token) h.authorization = `Bearer ${token}`;
  const r = await fetch(`${BASE}/api${path}`, { method, headers: h, body: body ? JSON.stringify(body) : undefined });
  return r.json().catch(() => ({}));
}
async function signup(email, role) {
  await api("/register", "POST", { email, password: "pw12345678", role });
  return (await api("/login", "POST", { email, password: "pw12345678" })).access_token;
}
const month = new Date().toISOString().slice(0, 7);
const day = (n) => `${month}-${String(n).padStart(2, "0")}`;

const admin = await signup("admin@acme.io", "admin");
const ada = await signup("ada@acme.io", "member");
const bo = await signup("bo@acme.io", "member");
await signup("boss@acme.io", "member");

const P = {};
for (const [key, name] of [["APOLLO", "Apollo"], ["ZEPHYR", "Zephyr"], ["ORION", "Orion"]])
  P[name] = (await api("/projects", "POST", { key, name }, admin)).id;
const C = {};
for (const name of ["engineering", "sales", "design", "ops"])
  C[name] = (await api("/categories", "POST", { name }, admin)).id;

// memberships: ada + bo on some projects; boss LEADS all three (team view).
const member = (proj, email, role) => api(`/projects/${P[proj]}/members`, "POST", { email, role }, admin);
await member("Apollo", "ada@acme.io", "member"); await member("Zephyr", "ada@acme.io", "member");
await member("Apollo", "bo@acme.io", "member"); await member("Orion", "bo@acme.io", "member");
for (const p of ["Apollo", "Zephyr", "Orion"]) await member(p, "boss@acme.io", "lead");
const boss = (await api("/login", "POST", { email: "boss@acme.io", password: "pw12345678" })).access_token;

async function log(tok, proj, cat, d, mins) {
  await api("/entries", "POST", { project: P[proj], category: C[cat], minutes: mins, day: day(d) }, tok);
}
const plan = [
  [ada, "Apollo", "engineering", [[3,120],[5,180],[9,150],[12,90],[16,200],[19,160],[23,120]]],
  [ada, "Zephyr", "design", [[6,60],[13,90],[20,75]]],
  [bo, "Apollo", "engineering", [[4,140],[10,160],[17,130],[24,110]]],
  [bo, "Orion", "sales", [[2,90],[8,120],[15,140],[22,100]]],
  [boss, "Zephyr", "ops", [[7,80],[14,100],[21,60]]],
  [boss, "Orion", "engineering", [[11,90],[18,110]]],
];
for (const [tok, proj, cat, days] of plan) for (const [d, m] of days) await log(tok, proj, cat, d, m);

// a couple of TODAY entries with a time-of-day, so boss's Calendar day view has
// blocks on the grid (start = minutes from midnight).
const todayStr = new Date().toISOString().slice(0, 10);
await api("/entries", "POST", { project: P.Zephyr, category: C.ops, minutes: 90, day: todayStr, start: 9 * 60 }, boss);
await api("/entries", "POST", { project: P.Orion, category: C.engineering, minutes: 120, day: todayStr, start: 13 * 60 }, boss);

// ---- drive the SPA on a phone viewport --------------------------------------
const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: 414, height: 896 },
  recordVideo: { dir: OUT, size: { width: 414, height: 896 } },
  deviceScaleFactor: 2, isMobile: true, hasTouch: true,
});
const page = await ctx.newPage();
await page.goto(BASE);

try {
  await page.getByPlaceholder("email").fill("boss@acme.io");
  await page.getByPlaceholder("password").fill("pw12345678");
  await page.getByRole("button", { name: "Log in" }).click();
  await page.getByRole("tab", { name: "Reports" }).waitFor({ timeout: 10000 });
  await sleep(900);

  // team distribution
  await page.getByRole("tab", { name: "Reports" }).click();
  await sleep(1200);
  await page.getByRole("button", { name: "Everyone" }).click();
  await sleep(2600);
  await page.getByRole("button", { name: "Week" }).click(); await sleep(1600);
  await page.getByRole("button", { name: "Year" }).click(); await sleep(1600);
  await page.getByRole("button", { name: "Month" }).click(); await sleep(1400);
  // scroll to reveal the per-day + per-person charts
  await page.mouse.wheel(0, 700); await sleep(2000);
  await page.mouse.wheel(0, 700); await sleep(1800);

  // Calendar day view — today's grid shows scheduled blocks; tap a slot to add one.
  await page.mouse.wheel(0, -1400); await sleep(300);
  await page.getByRole("tab", { name: "Calendar" }).click();
  await sleep(2000);
  await page.getByTestId("daygrid").click({ position: { x: 60, y: 250 } }); // ~11:00
  await sleep(1200);
  await page.getByRole("button", { name: "Save" }).click();
  await sleep(2200);

  // live pomodoro on the Log tab (project/category default to the first)
  await page.getByRole("tab", { name: "Log" }).click();
  await sleep(900);
  await page.getByRole("button", { name: /Start timer/ }).click();
  await sleep(3000);
  await page.getByRole("button", { name: /Stop/ }).click();
  await sleep(1600);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
