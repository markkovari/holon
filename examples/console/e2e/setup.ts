// Bring up the real stack the browser suite drives, then tear it down.
//
//   SurrealDB (pinned container)
//     ← comp-trace-seed   writes one run through `trace.rs`, the driver's own path
//   comp-host serving the composed console-domain
//     → knowledge:graph → SurrealDB
//
// Nothing below the browser is stubbed. That is the point: the run view reads the
// merged store through a wasm component, and a suite that mocked `/api/runs`
// would keep passing after the component stopped composing.
//
// ## It fails loudly rather than skipping quietly
//
// A Playwright suite that "passes" because the app never started is the worst
// outcome available — it reports green while proving nothing. So every
// prerequisite is checked and its absence throws with what to run.

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { createServer, type Server } from "node:http";
import { existsSync } from "node:fs";
import { createConnection } from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, "../../..");

// Pinned, like every other database fixture here: the answers asserted upstream
// were captured from this version, and `latest` turns a server upgrade into a
// mystery failure in a test nobody changed.
const SURREAL_IMAGE = "surrealdb/surrealdb:v3.1.3";
const SURREAL_PORT = 8111;
const CONSOLE_PORT = 3056;
const CONTAINER = "console-e2e-surreal";
const PLATFORM_PORT = 8113;
/// The token the stand-in issues. The console must carry it back as a bearer.
const TOKEN = "e2e-token";

const children: ChildProcess[] = [];
let platform: Server | undefined;

/// A stand-in `platform-domain`: enough of it to get a session.
///
/// The console inherits its auth from the platform (that was the decision — one
/// place decides who anyone is), so there is no session without one and no UI
/// without a session. An earlier version of this harness ran without a platform
/// on the theory that the run view reads the knowledge store directly and should
/// therefore work alone. It does read the store directly — but it is still
/// behind the login, and it should be: run history is operational data, not
/// something an anonymous visitor gets.
///
/// Deliberately not the real `platform-domain`: that would need a database, a
/// registered account and a lattice, and none of it is what these tests assert.
function standInPlatform(port: number): Promise<Server> {
  const server = createServer((req, res) => {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      res.setHeader("content-type", "application/json");

      // Everything except the login REQUIRES the bearer token. Not decoration:
      // a stand-in that answered anonymously would let the console believe it
      // already had a session, the login form would never render, and the suite
      // would silently stop testing the exchange it exists to test. That is
      // precisely what happened when this was written permissively.
      const authorized = req.headers.authorization === `Bearer ${TOKEN}`;

      if (req.url?.startsWith("/api/login")) {
        const { email } = JSON.parse(body || "{}");
        if (!email) {
          res.statusCode = 400;
          res.end(JSON.stringify({ error: "email required" }));
          return;
        }
        res.end(JSON.stringify({ token: TOKEN, subject: email }));
      } else if (!authorized) {
        // 401, which the console turns into `{authenticated: false}` — the
        // answer that makes the SPA render a login form rather than a fault.
        res.statusCode = 401;
        res.end(JSON.stringify({ error: "unauthorized" }));
      } else if (req.url?.startsWith("/api/me")) {
        res.end(JSON.stringify({ subject: "e2e@example.test" }));
      } else if (req.url?.includes("/goals")) {
        res.end(JSON.stringify({ goals: [] }));
      } else if (req.url?.startsWith("/api/projects")) {
        res.end(JSON.stringify({ projects: [{ id: "demo", name: "demo" }] }));
      } else {
        res.statusCode = 404;
        res.end(JSON.stringify({ error: "not_found" }));
      }
    });
  });
  return new Promise((resolve) => server.listen(port, "127.0.0.1", () => resolve(server)));
}

function bin(rel: string): string {
  const p = path.join(repo, rel);
  if (!existsSync(p)) {
    throw new Error(
      `missing ${rel} — run:\n` +
        `  cargo build --release --manifest-path reconciler/Cargo.toml --bin comp-trace-seed\n` +
        `  just build && just compose-console`,
    );
  }
  return p;
}

/// Wait until an HTTP endpoint answers, or give up saying what didn't.
///
/// NOT a TCP-accept check. SurrealDB accepts connections before it can serve
/// `/sql`, so a port probe returns while the next request still fails — which
/// showed up here as a seeder that worked on one run and failed on the next.
/// Poll the thing you actually need, which is the rule `Fleet::until` states for
/// the Rust side of this repo.
async function waitForHttp(url: string, what: string, ms = 60_000) {
  const deadline = Date.now() + ms;
  for (;;) {
    try {
      await fetch(url);
      return;
    } catch {
      // Not up yet.
    }
    if (Date.now() > deadline) throw new Error(`${what} never answered at ${url}`);
    await new Promise((r) => setTimeout(r, 250));
  }
}

/// Wait until something accepts a TCP connection, or give up saying what didn't.
async function waitForPort(port: number, what: string, ms = 60_000) {
  const deadline = Date.now() + ms;
  for (;;) {
    const open = await new Promise<boolean>((resolve) => {
      const s = createConnection({ port, host: "127.0.0.1" })
        .on("connect", () => {
          s.destroy();
          resolve(true);
        })
        .on("error", () => resolve(false));
    });
    if (open) return;
    if (Date.now() > deadline) throw new Error(`${what} never came up on :${port}`);
    await new Promise((r) => setTimeout(r, 250));
  }
}

export default async function globalSetup() {
  // --- SurrealDB ------------------------------------------------------------
  try {
    execFileSync("docker", ["rm", "-f", CONTAINER], { stdio: "ignore" });
  } catch {
    // Not running. Fine — this is the "clean up a previous crashed run" case.
  }
  execFileSync(
    "docker",
    [
      "run", "--rm", "-d", "--name", CONTAINER,
      // Loopback only: a test database must not become reachable from the
      // network just because a suite is running.
      "-p", `127.0.0.1:${SURREAL_PORT}:8000`,
      SURREAL_IMAGE,
      // Unauthenticated on purpose. `comp-host` fetches granted secrets from a
      // platform (ADR-0051) and there is no flag to hand it one, so the graph
      // component would reach the database with no password. `goalrun` already
      // documents this as a supported local setup ("absent means the server
      // takes unauthenticated writes"), and this is that setup — bound to
      // loopback, in a container that dies with the suite.
      "start", "--no-banner", "--unauthenticated",
      "--bind", "0.0.0.0:8000", "memory",
    ],
    { stdio: "inherit" },
  );
  await waitForHttp(`http://127.0.0.1:${SURREAL_PORT}/health`, SURREAL_IMAGE);

  // --- the stand-in platform, for the session --------------------------------
  platform = await standInPlatform(PLATFORM_PORT);

  // --- one run, written by the driver's own code path ------------------------
  execFileSync(
    bin("reconciler/target/release/comp-trace-seed"),
    ["--surreal-url", `http://127.0.0.1:${SURREAL_PORT}`],
    { stdio: "inherit" },
  );

  // --- the console -----------------------------------------------------------
  // `comp-plug` derives the composition from the component's own imports, so the
  // suite always runs against what the build actually produces rather than a
  // path someone remembered to update.
  const composed = execFileSync(bin("reconciler/target/release/comp-plug"), ["console-domain"], {
    cwd: repo,
    encoding: "utf8",
  }).trim();

  const host = spawn(
    bin("host/target/release/comp-host"),
    [
      "--app", "console",
      "--config", "default-tenant=console",
      // The stand-in platform, for the session. The run view reads the knowledge
      // store directly (ADR-0091 keeps run history out of the control plane),
      // but it is still behind the login like everything else here.
      "--config", `platform-url=http://127.0.0.1:${PLATFORM_PORT}`,
      "--config", `surreal-url=http://127.0.0.1:${SURREAL_PORT}`,
      "--config", "surreal-ns=comp",
      "--config", "surreal-db=goalmemory",
      "--config", "surreal-user=root",
      // The store is on loopback, which the host denies by default (ADR-0008).
      "--egress", `127.0.0.1:${SURREAL_PORT}`,
      "--egress", `127.0.0.1:${PLATFORM_PORT}`,
      "--allow-private-egress",
      "--component", composed,
      "--addr", `127.0.0.1:${CONSOLE_PORT}`,
    ],
    { cwd: repo, stdio: "inherit" },
  );
  children.push(host);
  await waitForHttp(`http://127.0.0.1:${CONSOLE_PORT}/`, "the console");

  return async () => {
    for (const c of children) c.kill();
    platform?.close();
    try {
      execFileSync("docker", ["rm", "-f", CONTAINER], { stdio: "ignore" });
    } catch {
      // Already gone.
    }
  };
}
