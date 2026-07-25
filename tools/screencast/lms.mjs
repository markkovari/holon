// Screencast: the lms learning platform (React + shadcn UI) on the REAL running
// app. Seeds an instructor (+ demo course) via the API, then records the STUDENT
// flow — enroll, read a lesson, answer the multiple-choice quiz to 100% (a
// Certificate unlocks) — then logs in as the instructor to show the gradebook
// (table + a server-rendered class-average chart).
//
// Prereq: from repo root  `just host-lms &`   (builds the UI, serves on :3048)
import { chromium } from "playwright";

const BASE = process.env.LMS_URL || "http://127.0.0.1:3048";
const OUT = new URL("./videos/lms/", import.meta.url).pathname;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// seed an instructor (registering as instructor seeds the demo course).
async function api(path, body) {
  await fetch(`${BASE}/api${path}`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
}
await api("/register", { email: "prof@acme.io", password: "pw12345678", role: "instructor" });

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: 900, height: 860 },
  recordVideo: { dir: OUT, size: { width: 900, height: 860 } },
  deviceScaleFactor: 1,
});
const page = await ctx.newPage();
await page.goto(BASE);

async function signIn(email, role) {
  await page.getByPlaceholder("email").fill(email);
  await page.getByPlaceholder("password").fill("pw12345678");
  await page.locator("button[role=combobox]").click(); await sleep(200);
  await page.getByRole("option", { name: role }).click();
}

try {
  // ---- student: enroll, take the quiz to 100% ----
  await signIn("stu@acme.io", "student");
  await page.getByRole("button", { name: "Register" }).click();
  await page.getByText("Course catalog").waitFor({ timeout: 10000 });
  await sleep(900);
  await page.getByRole("button", { name: "Enroll" }).first().click();
  await sleep(1400);
  // open a lesson
  await page.getByText("What is a component?").click(); await sleep(1300);
  // answer correctly (options idx 1,0,1 -> radios 1,3,7 of 3-per-question)
  const r = page.locator("input[type=radio]");
  await r.nth(1).check(); await sleep(300);
  await r.nth(3).check(); await sleep(300);
  await r.nth(7).check(); await sleep(400);
  await page.getByRole("button", { name: /Submit/ }).click();
  await sleep(2000); // 100% + progress + certificate
  await page.mouse.wheel(0, -600); await sleep(1600); // back to the progress bar / certificate

  // ---- instructor: the gradebook ----
  await page.getByTitle("Log out").click(); await sleep(700);
  await signIn("prof@acme.io", "instructor");
  await page.getByRole("button", { name: "Log in" }).click();
  await page.getByText("My courses").waitFor({ timeout: 10000 });
  await sleep(800);
  await page.getByRole("button", { name: /WIT101/ }).click();
  await sleep(1600);
  await page.mouse.wheel(0, 500); await sleep(2600); // the gradebook table + chart
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
