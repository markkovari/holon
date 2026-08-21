// Screencast: the photosocial showcase (AI photo critique + RBAC attribute ratings)
// on the REAL running app.
//
// Records the full user experience:
// 1. Admin logs in and configures custom evaluation criteria in Admin Studio.
// 2. Creator uploads photographic artwork and AI generates automated critique and narrative.
// 3. Community voter upvotes the photo and scores attributes via interactive sliders.
// 4. Feed updates live with aggregate mean scores and AI narrative.
//
// Usage: from repo root `just screencast-photosocial`

import { chromium } from "playwright";
import { spawn } from "child_process";
import { fileURLToPath } from "url";
import { dirname, join } from "path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, "../..");
const OUT = join(__dirname, "videos/photosocial/");
const PORT = 3055;
const BASE = `http://127.0.0.1:${PORT}`;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function waitForServer() {
  for (let i = 0; i < 60; i++) {
    try {
      const res = await fetch(`${BASE}/api/info`);
      if (res.ok) return;
    } catch (_) {}
    await sleep(100);
  }
  throw new Error("Server did not start in time");
}

let hostProcess = null;

try {
  // Check if server is already running, else spawn comp-host
  let isRunning = false;
  try {
    const res = await fetch(`${BASE}/api/info`);
    isRunning = res.ok;
  } catch (_) {}

  if (!isRunning) {
    console.log("Spawning comp-host on port", PORT);
    hostProcess = spawn(
      join(ROOT, "host/target/release/comp-host"),
      [
        "--component",
        join(ROOT, "components/target/photosocial_domain.composed.wasm"),
        "--addr",
        `127.0.0.1:${PORT}`,
        "--kv",
        "memory",
      ],
      { env: { ...process.env, VET_TENANT: "photosocial" }, stdio: "ignore" }
    );
    await waitForServer();
  }

  const browser = await chromium.launch({ headless: true });
  const ctx = await browser.newContext({
    viewport: { width: 820, height: 820 },
    recordVideo: { dir: OUT, size: { width: 820, height: 820 } },
    deviceScaleFactor: 1,
  });
  const page = await ctx.newPage();
  await page.goto(BASE);
  await sleep(1000);

  // 1. Admin login & open Admin Studio
  await page.locator("#authBtn").click();
  await sleep(600);
  await page.locator("#authEmail").fill("admin@holon.test");
  await page.locator("#authPassword").fill("admin1234");
  await page.locator("#authRole").selectOption("admin");
  await sleep(400);
  await page.getByRole("button", { name: "Sign In" }).click();
  await sleep(1000);

  // 2. Open Admin Studio & create a new attribute
  await page.locator("#adminBtn").click();
  await sleep(800);
  await page.locator("#newAttrName").fill("Storytelling & Mood");
  await page.locator("#newAttrDesc").fill("Emotional resonance, atmosphere, and visual narrative.");
  await sleep(600);
  await page.getByRole("button", { name: "Add Admin Attribute" }).click();
  await sleep(1200);
  await page.locator("#adminModal .close-btn").click();
  await sleep(800);

  // 3. Creator signs in & uploads photo
  await page.locator("#authBtn").click();
  await sleep(500);
  await page.locator("#authEmail").fill("elena.camerawork@holon.test");
  await page.locator("#authPassword").fill("creator1234");
  await page.locator("#authRole").selectOption("user");
  await sleep(400);
  await page.getByRole("button", { name: "Register" }).click();
  await sleep(1200);

  // Upload photo
  await page.locator("#uploadBtn").click();
  await sleep(600);
  await page.locator("#uploadTitle").fill("Midnight Tokyo Reflections");
  await page.locator("#uploadImgUrl").fill("https://images.unsplash.com/photo-1503899036084-c55cdd92da26?w=800");
  await page.locator("#uploadDesc").fill("Captured with 50mm prime f/1.4 during heavy neon rain. Focused on wet road reflection geometry.");
  await sleep(800);
  await page.getByRole("button", { name: "Upload & Request AI Critique" }).click();
  await sleep(1800);

  // 4. Community voter upvotes photo
  const upvoteBtn = page.locator(".vote-btn").first();
  await upvoteBtn.click();
  await sleep(800);

  // 5. Open Photo Details to view AI Critique and rate attributes
  const reviewBtn = page.getByRole("button", { name: "Review & Rate →" }).first();
  await reviewBtn.click();
  await sleep(1600);

  // Adjust attribute rating sliders
  const sliders = page.locator(".rating-slider");
  const count = await sliders.count();
  if (count > 0) {
    await sliders.nth(0).evaluate((el) => { el.value = "9.5"; el.dispatchEvent(new Event('input')); });
    await sleep(500);
  }
  if (count > 1) {
    await sliders.nth(1).evaluate((el) => { el.value = "9"; el.dispatchEvent(new Event('input')); });
    await sleep(500);
  }
  if (count > 2) {
    await sliders.nth(2).evaluate((el) => { el.value = "9.5"; el.dispatchEvent(new Event('input')); });
    await sleep(500);
  }
  if (count > 3) {
    await sliders.nth(3).evaluate((el) => { el.value = "9"; el.dispatchEvent(new Event('input')); });
    await sleep(500);
  }
  await sleep(1000);

  // Submit attribute ratings
  await page.getByRole("button", { name: "Submit Ratings" }).click();
  await sleep(2500);

  await ctx.close();
  await browser.close();
  console.log("Recorded photosocial screencast successfully");
} finally {
  if (hostProcess) {
    hostProcess.kill();
  }
}
