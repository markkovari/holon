// Screencast: the durable job queue running on the wasmCloud v2 operator on
// Kubernetes, with the GOLEM execution backend live. Each job runs as a real
// durable Golem worker (a persistent counter): a burst of "email" jobs completes
// with results 1..5, a second burst continues 6..10 (the counter is durable —
// state accumulates in Golem across jobs), and other types get their own
// counters. Nothing is faked — this drives the real board served by the
// composed component on the v2 host, whose golem-bridge calls Golem over
// wasi:http.
//
// Prereq: the k8s stack is up (just k8s-jobs) and Golem + the CounterAgent are
// reachable (see docs/apps/JOBS.md). Board: http://jobs.jobs.svc.cluster.local
import { chromium } from "playwright";

const BASE = process.env.JOBS_URL || "http://jobs.jobs.svc.cluster.local";
const OUT = new URL("./videos/jobs-golem/", import.meta.url).pathname;
const W = 1200, H = 680;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
  deviceScaleFactor: 2,
});
const page = await ctx.newPage();
await page.goto(BASE);

const enqueue = (type) =>
  page.evaluate(
    (t) => fetch("/api/jobs", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ type: t }) }),
    type,
  );

try {
  await page.locator("#status").filter({ hasText: "live" }).waitFor({ timeout: 12000 });
  await sleep(900);

  // 1. enqueue "shipment" jobs one at a time -> each runs as a durable Golem
  //    worker; the shipment counter climbs 1,2,3,... in the Done column.
  for (let i = 0; i < 8; i++) {
    await enqueue("shipment");
    await sleep(650);
  }
  await sleep(2500);

  // 2. a different type -> its OWN durable counter, starting at 1 — per-workflow
  //    durable state, all persisted in Golem.
  for (let i = 0; i < 3; i++) {
    await enqueue("invoice");
    await sleep(650);
  }
  await sleep(3000);

  // hold on the board: Done cards showing the durable Golem counter values.
  await sleep(2000);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
