// Scores the SHIPPED matcher against the case set — the console's own file, not
// a copy of it. Node 24 strips the types, so there is no build step and no way
// for the thing benchmarked to drift from the thing served.
//
//   node bench/needle/baseline.mjs
import { readFileSync } from "node:fs";
import { parse } from "../../examples/console/ui/src/query.ts";

const { titles, cases } = JSON.parse(readFileSync(new URL("cases.json", import.meta.url)));
let ok = 0;
const t0 = performance.now();
for (const { q, want } of cases) {
  const got = parse(q, titles);
  const hit =
    got.kind === want.kind &&
    (want.state ?? got.state) === got.state &&
    (want.title ?? got.title) === got.title;
  if (hit) ok++;
  else console.log(`  MISS ${JSON.stringify(q)} -> ${JSON.stringify(got)} want ${JSON.stringify(want)}`);
}
const ms = performance.now() - t0;
console.log(`baseline ${ok}/${cases.length}  ${(ms / cases.length).toFixed(3)}ms/query  0 deps`);
