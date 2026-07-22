// Screencast: search-as-you-type. Type a query and watch ranked hits narrow
// live, toggle all-mode to intersect, click a facet to filter, then repeat a
// query to see the ⚡ cached badge + hit-ratio climb — the read/query axis in a
// real browser.
import { chromium } from "playwright";

const BASE = process.env.SEARCH_URL || "http://127.0.0.1:3019";
const OUT = new URL("./videos/search/", import.meta.url).pathname;
const W = 900, H = 760;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
});
const page = await ctx.newPage();
await page.goto(BASE);

const type = async (text) => {
  await page.fill("#q", "");
  for (const ch of text) { await page.type("#q", ch, { delay: 90 }); }
};

try {
  await sleep(1200); // corpus seeds on load

  // type-as-you-go: ranked hits narrow with each keystroke.
  await type("distributed");
  await sleep(1600);

  // add a term + all-mode: intersection sharpens the set.
  await type("distributed saga");
  await sleep(1200);
  await page.click('[data-mode="all"]');
  await sleep(1600);

  // facet filter.
  await page.click('[data-mode="any"]');
  await type("index");
  await sleep(900);
  await page.click('[data-facet="topic:search"]');
  await sleep(1600);

  // repeat a query -> ⚡ cached + hit-ratio climbs.
  await page.click('[data-facet="topic:search"]'); // clear facet
  await type("encryption");
  await sleep(900);
  await type("encryption"); // identical repeat
  await sleep(1800);
} finally {
  await ctx.close();
  await browser.close();
}
console.log("done");
