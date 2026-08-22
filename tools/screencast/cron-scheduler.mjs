import { chromium } from "playwright";
import { spawn } from "child_process";
import { fileURLToPath } from "url";
import { dirname, join } from "path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, "../..");
const OUT = join(__dirname, "videos/cron-scheduler/");
const PORT = 3056;
const BASE = `http://127.0.0.1:${PORT}`;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function waitForServer() {
  for (let i = 0; i < 60; i++) {
    try {
      const res = await fetch(`${BASE}/`);
      if (res.ok) return;
    } catch (_) {}
    await sleep(100);
  }
  throw new Error("Server did not start in time");
}

let hostProcess = null;

try {
  hostProcess = spawn(
    join(ROOT, "host/target/release/comp-host"),
    [
      "--app", "cron-scheduler",
      "--config-file", join(ROOT, "examples/defaults.conf"),
      "--config", "default-tenant=cron-scheduler",
      "--component", join(ROOT, "components/target/cron-scheduler.composed.wasm"),
      "--addr", `127.0.0.1:${PORT}`
    ],
    { stdio: "ignore" }
  );
  await waitForServer();

  const browser = await chromium.launch({ headless: true });
  const ctx = await browser.newContext({
    viewport: { width: 820, height: 820 },
    recordVideo: { dir: OUT, size: { width: 820, height: 820 } },
    deviceScaleFactor: 1,
  });
  const page = await ctx.newPage();
  await page.goto(BASE);
  await sleep(1000);

  await page.getByRole('button').first().click();
  await sleep(2000);

  await ctx.close();
  await browser.close();
} finally {
  if (hostProcess) {
    hostProcess.kill();
  }
}
