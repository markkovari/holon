// Screencast: the Holon console — sign in, the worklist, and a run's history.
//
// The story it tells: a run is no longer a thing that scrolls off a terminal.
// Sign in, look at the worklist, open the runs tab, and read what actually
// happened inside one — both branches kept, the loser's score beside the
// winner's, the gate's verdicts in order, and the capability the pool was
// missing (ADR-0089, ADR-0092).
//
// ## Why this one starts its own stack
//
// Every other recorder here assumes `just host-<app>` is already running, because
// every other app is one component and one port. The console needs three things
// on the other side — the knowledge store it reads runs from, a platform to
// authenticate against, and the composed component itself — so a recorder that
// assumed them would be a README step nobody performs correctly at 11pm.
//
// The platform is a stand-in for the same reason the browser suite uses one: a
// real `platform-domain` needs a database, a registered account and a lattice,
// and none of that is what this GIF is about.
//
//   node console.mjs
//   bash to-gif.sh videos/console/*.webm ../../docs/media/console.gif 900 12

import { execFileSync, spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { createServer } from "node:http";
import path from "node:path";
import { chromium } from "playwright";

const repo = path.resolve(new URL(".", import.meta.url).pathname, "../..");
const OUT = new URL("./videos/console/", import.meta.url).pathname;
// 880 tall. The run detail no longer fits in one frame — the branches are a
// graph now, and a graph short enough to fit above the timeline is a graph you
// cannot read — so the recording scrolls once instead of growing the crop. A
// taller frame would just make the whole GIF smaller on the page.
const W = 1200, H = 880;

const SURREAL_IMAGE = "surrealdb/surrealdb:v3.1.3";
const SURREAL_PORT = 8121;
const PLATFORM_PORT = 8122;
const CONSOLE_PORT = 3061;
const CONTAINER = "console-screencast-surreal";
const TOKEN = "screencast-token";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function bin(rel) {
  const p = path.join(repo, rel);
  if (!existsSync(p)) throw new Error(`missing ${rel} — run \`just compose-console\` first`);
  return p;
}

async function waitForHttp(url, what, ms = 60_000) {
  const deadline = Date.now() + ms;
  for (;;) {
    try {
      await fetch(url);
      return;
    } catch {
      /* not up yet */
    }
    if (Date.now() > deadline) throw new Error(`${what} never answered at ${url}`);
    await sleep(250);
  }
}

async function surql(body) {
  const r = await fetch(`http://127.0.0.1:${SURREAL_PORT}/sql`, {
    method: "POST",
    headers: { accept: "application/json", "surreal-ns": "comp", "surreal-db": "goalmemory" },
    body,
  });
  if (!r.ok) throw new Error(`seeding failed: ${await r.text()}`);
}

// A platform that only does what the console asks of it. Everything but the
// login requires the bearer, so the login screen is real rather than skipped.
function standInPlatform() {
  const server = createServer((req, res) => {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      if (process.env.SCREENCAST_DEBUG) console.error(`[platform] ${req.method} ${req.url}`);
      res.setHeader("content-type", "application/json");
      const ok = req.headers.authorization === `Bearer ${TOKEN}`;
      if (req.url?.startsWith("/api/login")) {
        res.end(JSON.stringify({ token: TOKEN, subject: JSON.parse(body || "{}").email }));
      } else if (!ok) {
        res.statusCode = 401;
        res.end(JSON.stringify({ error: "unauthorized" }));
      } else if (req.url?.startsWith("/api/me")) {
        // The console asks this immediately after the login to confirm the
        // cookie works. Omitting it 404s a request the SPA treats as fatal, and
        // the screen stays on the login form saying "not_found".
        res.end(JSON.stringify({ subject: "you@holon.dev" }));
      } else if (req.url?.includes("/goals")) {
        res.end(
          JSON.stringify({
            goals: [
              { id: "g1", title: "Paginate the search results", state: "queued" },
              { id: "g2", title: "Redact PII from exported CSVs", state: "running" },
              { id: "g3", title: "Cache the capability graph", state: "done" },
            ],
          }),
        );
      } else if (req.url?.startsWith("/api/projects")) {
        res.end(JSON.stringify({ projects: [{ id: "holon", name: "holon" }] }));
      } else {
        res.statusCode = 404;
        res.end(JSON.stringify({ error: "not_found" }));
      }
    });
  });
  return new Promise((resolve) => server.listen(PLATFORM_PORT, "127.0.0.1", () => resolve(server)));
}

let platform;
let host;

async function up() {
  try {
    execFileSync("docker", ["rm", "-f", CONTAINER], { stdio: "ignore" });
  } catch {
    /* nothing to clean */
  }
  execFileSync("docker", [
    "run", "--rm", "-d", "--name", CONTAINER,
    "-p", `127.0.0.1:${SURREAL_PORT}:8000`,
    SURREAL_IMAGE,
    "start", "--no-banner", "--unauthenticated", "--bind", "0.0.0.0:8000", "memory",
  ], { stdio: "ignore" });
  await waitForHttp(`http://127.0.0.1:${SURREAL_PORT}/health`, SURREAL_IMAGE);

  // The run the GIF opens, written by `trace.rs` — the driver's own code path,
  // so what is on screen is the shape a real run records.
  execFileSync(bin("reconciler/target/release/comp-trace-seed"), [
    "--surreal-url", `http://127.0.0.1:${SURREAL_PORT}`,
  ], { stdio: "ignore" });

  // Two more, so the list is a worklist rather than one row. Written directly
  // because they are scenery: an exhausted run and one still going, which are
  // the two states the seeder does not produce.
  await surql(`
    UPSERT run:⟨76/g1⟩ SET id_text = '76/g1', goal = 'Redact PII from exported CSVs',
      outcome = 'exhausted', branches = 4, started_at = time::now(), resolved_at = time::now();
    UPSERT run:⟨75/g1⟩ SET id_text = '75/g1', goal = 'Cache the capability graph',
      outcome = NONE, branches = 4, started_at = time::now(), resolved_at = NONE;
  `);

  platform = await standInPlatform();

  const composed = execFileSync(bin("reconciler/target/release/comp-plug"), ["console-domain"], {
    cwd: repo, encoding: "utf8",
  }).trim();

  host = spawn(bin("host/target/release/comp-host"), [
    "--app", "console",
    "--config", "default-tenant=console",
    "--config", `platform-url=http://127.0.0.1:${PLATFORM_PORT}`,
    "--config", `surreal-url=http://127.0.0.1:${SURREAL_PORT}`,
    "--config", "surreal-ns=comp",
    "--config", "surreal-db=goalmemory",
    "--config", "surreal-user=root",
    "--egress", `127.0.0.1:${SURREAL_PORT}`,
    "--egress", `127.0.0.1:${PLATFORM_PORT}`,
    "--allow-private-egress",
    "--component", composed,
    "--addr", `127.0.0.1:${CONSOLE_PORT}`,
  ], { cwd: repo, stdio: "ignore" });
  await waitForHttp(`http://127.0.0.1:${CONSOLE_PORT}/`, "the console");
}

function down() {
  host?.kill();
  platform?.close();
  try {
    execFileSync("docker", ["rm", "-f", CONTAINER], { stdio: "ignore" });
  } catch {
    /* already gone */
  }
}

await up();

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
});
const page = await ctx.newPage();

try {
  await page.goto(`http://127.0.0.1:${CONSOLE_PORT}/`);
  await sleep(1200);

  // Sign in. Typed rather than filled, because the point of the first beat is
  // that this is a real login against the platform, not a screenshot.
  await page.getByPlaceholder("email").pressSequentially("you@holon.dev", { delay: 55 });
  await sleep(300);
  // A real string, not bullet characters: `pressSequentially` sends key events
  // and U+2022 is not typeable, so the field stayed empty and the form never
  // submitted. The input masks it on screen regardless.
  await page.getByPlaceholder("password").pressSequentially("correct-horse", { delay: 45 });
  await sleep(500);
  await page.getByRole("button", { name: "Sign in" }).click();
  await sleep(2000);

  // The worklist: goals queued, running, done. Nothing starts itself.
  await sleep(1800);

  // The runs tab — what actually happened, after the terminal closed.
  await page.getByTestId("tab-runs").click();
  await sleep(2200);

  // Open the run that merged. The graph is the point: two branches in ONE round
  // is a fan-out, and the flat list this replaced could not say that.
  await page.getByTestId("run-77/g1").click();
  await sleep(3200);

  // Click the winner. The panel is what the flat list used to be — cost, paths,
  // its own events — now for the one branch somebody asked about.
  await page.getByTestId("run-graph").getByText("mvp", { exact: true }).click();
  await sleep(3000);

  // Down to the timeline, where that branch's rows are lit rather than filtered:
  // the interleaving stays, which is the only thing on the page that shows the
  // two branches ran at the same time.
  await page.mouse.wheel(0, 420);
  await sleep(3200);
  await page.mouse.wheel(0, -420);
  await sleep(900);

  // Back out to the list, so the last frame is the thing you would open again.
  await page.getByText("← all runs").click();
  await sleep(1800);
} catch (e) {
  // A recorder that dies with a locator timeout says nothing about WHY the page
  // was not what it expected. The screenshot is the difference between "it broke"
  // and "the login failed", which are two different mornings.
  await page.screenshot({ path: `${OUT}failure.png` }).catch(() => {});
  console.error(`\nrecording failed on: ${e.message.split("\n")[0]}`);
  console.error(`page said: ${(await page.locator("body").innerText().catch(() => "?")).replace(/\n+/g, " | ").slice(0, 300)}`);
  throw e;
} finally {
  await ctx.close();
  await browser.close();
  down();
}
console.log("done");
