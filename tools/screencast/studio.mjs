// Screencast: the composition studio (React + xyflow) on the REAL running app,
// with the repo's own 109 components in the palette. The story, in order:
//
//   1. filter the palette and place four components — each node's handles are its
//      real interfaces, read out of the binary by `wit:reflect`
//   2. wire the three imports; the plan flips from "Unsatisfied (3)" to (0) and
//      every import handle goes from amber ("needs") to blue
//   3. read the same graph as a `wac plug` script, a declarative `.wac` file, and
//      a wasmCloud v2 WorkloadDeployment — three different deployment models
//   4. hit Compose and get a real composed component back
//
// Prereq: from repo root  `just host-studio &`  (SPA + seeded palette on :3054)
import { chromium } from "playwright";

const BASE = process.env.STUDIO_URL || "http://127.0.0.1:3054";
const OUT = new URL("./videos/studio/", import.meta.url).pathname;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: 1440, height: 860 },
  recordVideo: { dir: OUT, size: { width: 1440, height: 860 } },
  acceptDownloads: true,
  deviceScaleFactor: 1,
});
const page = await ctx.newPage();
await page.goto(BASE);

const place = async (name) => {
  await page.getByRole("button", { name: new RegExp(`^${name}`) }).first().click();
  await sleep(450);
};

/// Drag an export handle onto the matching import handle. The handle ids ARE the
/// interface names, so this is the same check the UI enforces while dragging.
const wire = async (plug, iface) => {
  const src = page.locator(`.react-flow__node[data-id="${plug}"] .react-flow__handle-right`).first();
  const dst = page.locator(`.react-flow__node[data-id="mesh-domain"] .react-flow__handle-left[data-handleid="${iface}"]`);
  const a = await src.boundingBox();
  const z = await dst.boundingBox();
  if (!a || !z) throw new Error(`no handle for ${plug} / ${iface}`);
  await page.mouse.move(a.x + a.width / 2, a.y + a.height / 2);
  await page.mouse.down();
  await page.mouse.move(z.x + z.width / 2, z.y + z.height / 2, { steps: 18 });
  await page.mouse.up();
  await sleep(900);
};

try {
  await sleep(1200);
  // 1. the palette is the whole repo, reflected
  await page.getByPlaceholder(/components/).fill("mesh");
  await sleep(900);
  await place("mesh-domain");
  await page.getByPlaceholder(/components/).fill("");
  await sleep(500);
  for (const c of ["record-store", "resilience", "proxy-route"]) await place(c);
  await sleep(900);

  // 2. wire it up — watch Unsatisfied (3) -> (0)
  await wire("record-store", "records:store/store@0.1.0");
  await wire("resilience", "resilience:breaker/breaker@0.1.0");
  await wire("proxy-route", "proxy:route/router@0.1.0");
  await page.getByRole("button", { name: "Arrange" }).click();
  await sleep(1600);

  // 3. the same graph, three ways
  for (const tab of ["wac plug", ".wac", "workload"]) {
    await page.getByRole("button", { name: tab, exact: true }).click();
    await sleep(2600);
  }
  await page.getByRole("button", { name: "plan", exact: true }).click();
  await sleep(800);

  // 4. compose for real
  const download = page.waitForEvent("download").catch(() => null);
  await page.getByRole("button", { name: "Compose" }).click();
  await download;
  await sleep(2600);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
