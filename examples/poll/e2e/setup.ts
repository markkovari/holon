// Bring up the real stack the browser suite drives, then tear it down.
//
//   comp-host serving the composed poll-domain
//     → records:store (in-memory kv) + svg:chart + qr:encode, all wasm
//
// Nothing below the browser is stubbed. That is the point: the chart is an SVG a
// component rendered and the one-vote rule is a cookie the component set, and a
// suite that mocked `/api/polls` would keep passing after either stopped working.
//
// ## It fails loudly rather than skipping quietly
//
// A Playwright suite that "passes" because the app never started is the worst
// outcome available — it reports green while proving nothing. So every prerequisite
// is checked and its absence throws with the command that fixes it.

import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, "../../..");

const PORT = Number(process.env.POLL_PORT ?? 3057);
const children: ChildProcess[] = [];

function need(rel: string, fix: string): string {
  const p = path.join(repo, rel);
  if (!existsSync(p)) {
    throw new Error(`missing ${rel} — run \`${fix}\` first`);
  }
  return p;
}

async function waitForHttp(url: string, what: string, tries = 60): Promise<void> {
  for (let i = 0; i < tries; i++) {
    try {
      const r = await fetch(url);
      if (r.ok) return;
    } catch {
      // Not up yet.
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`${what} never answered ${url}`);
}

export default async function globalSetup() {
  const host = need("host/target/release/comp-host", "just build-reconciler");
  // The COMPOSED artifact: poll-domain plus the four components that satisfy its
  // imports. `comp-plug` derives that chain from the imports themselves, so this is
  // one file rather than a list the suite has to keep in step.
  const composed = need("components/target/poll_domain.composed.wasm", "just compose-poll");

  const proc = spawn(
    host,
    [
      "--app", "poll",
      "--config", "default-tenant=poll",
      "--component", composed,
      "--addr", `127.0.0.1:${PORT}`,
    ],
    // NOT `stdio: "inherit"`. The host writes its startup banner to stdout, which is
    // where Playwright's json reporter writes the report — so an inherited stdout
    // produces a report that starts `comp-host: serving …` and will not parse. Both
    // streams go to stderr instead: still visible when a test fails, never mixed
    // into a machine-readable stdout.
    { cwd: repo, stdio: ["ignore", "pipe", "pipe"] },
  );
  proc.stdout?.pipe(process.stderr);
  proc.stderr?.pipe(process.stderr);
  children.push(proc);
  await waitForHttp(`http://127.0.0.1:${PORT}/health`, "the poll app");

  return async () => {
    for (const c of children) c.kill();
  };
}
