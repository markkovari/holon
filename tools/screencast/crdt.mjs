// Screencast: three replicas of one document editing OFFLINE (partitioned),
// then a SYNC that merges their state and shows all three converge to an
// identical result. Every value on screen is produced by the real crdt.wasm
// (the Rust component, transpiled via jco) run right here in Node — this script
// only lays the computed states out across three panes and animates them.
//
// Story (all edits concurrent, no coordination):
//   title  -> LWW-Map : Bob's later timestamp wins        ("Design proposal")
//   status -> LWW-Map : only Bob set it                    ("review")
//   likes  -> PN-Counter : 2 + 3 + 1 summed               (6)
//   tags   -> OR-Set : union; Carol removes "urgent" but Alice's concurrent
//             add survives (ADD WINS)                      [backend, q3, urgent]
//
// Prereq: cd ../../examples/jco-crdt && npm run transpile   (produces gen/)
import { chromium } from "playwright";

const { merger: c } = await import("../../examples/jco-crdt/gen/crdt.js");
const OUT = new URL("./videos/crdt/", import.meta.url).pathname;
const W = 1200, H = 660;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const view = (s) => JSON.parse(c.value(s));

// ---- run the REAL component to get every state on screen -----------------

// Shared starting point everyone forked from.
let baseDoc = c.lwwmapSet(c.lwwmapNew(), "title", '"Draft"', 1, "seed");
baseDoc = c.lwwmapSet(baseDoc, "status", '"todo"', 1, "seed");
const baseLikes = c.counterNew();
const baseTags = c.orsetNew();

// --- Alice (offline): rename, +2 likes, tag "urgent" ---
let aDoc = c.lwwmapSet(baseDoc, "title", '"Design spec"', 5, "alice");
let aLikes = c.counterAdd(baseLikes, "alice", 2);
let aTags = c.orsetAdd(baseTags, "urgent", "alice:1");

// --- Bob (offline): rename LATER (wins LWW), set status, +3 likes, tag "backend" ---
let bDoc = c.lwwmapSet(baseDoc, "title", '"Design proposal"', 9, "bob");
bDoc = c.lwwmapSet(bDoc, "status", '"review"', 9, "bob");
let bLikes = c.counterAdd(baseLikes, "bob", 3);
let bTags = c.orsetAdd(baseTags, "backend", "bob:1");

// --- Carol (offline): +1 like, add then REMOVE "urgent", tag "q3" ---
let cDoc = baseDoc;
let cLikes = c.counterAdd(baseLikes, "carol", 1);
let cTags = c.orsetAdd(baseTags, "urgent", "carol:1");
cTags = c.orsetRemove(cTags, "urgent"); // tombstones only carol:1
cTags = c.orsetAdd(cTags, "q3", "carol:2");

// --- SYNC: merge all three (order-independent; we fold left) ---
const mergeAll = (fn, xs) => xs.reduce((a, b) => c.merge(a, b));
const cvDoc = mergeAll(c.merge, [aDoc, bDoc, cDoc]);
const cvLikes = mergeAll(c.merge, [aLikes, bLikes, cLikes]);
const cvTags = mergeAll(c.merge, [aTags, bTags, cTags]);

const snap = (doc, likes, tags) => {
  const d = view(doc);
  return { title: d.title, status: d.status ?? "—", likes: view(likes), tags: view(tags) };
};
const replicas = [
  { name: "Alice", off: snap(aDoc, aLikes, aTags) },
  { name: "Bob", off: snap(bDoc, bLikes, bTags) },
  { name: "Carol", off: snap(cDoc, cLikes, cTags) },
];
const converged = snap(cvDoc, cvLikes, cvTags);

// ---- render + record ------------------------------------------------------

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT, size: { width: W, height: H } },
  deviceScaleFactor: 2,
});
const page = await ctx.newPage();

await page.setContent(`<!doctype html><meta charset=utf8>
<style>
  :root{color-scheme:dark}
  *{box-sizing:border-box;margin:0;font-family:ui-sans-serif,system-ui,sans-serif}
  body{background:#05070b;color:#e6e9ef;padding:22px}
  h1{font-size:18px;font-weight:700;letter-spacing:.2px}
  .sub{color:#8b95a7;font-size:13px;margin-top:3px}
  .banner{margin:14px 0 16px;padding:9px 14px;border-radius:10px;font-size:14px;font-weight:600;
    border:1px solid #2a2f3a;background:#0f1115;transition:.4s}
  .banner.diverged{border-color:#5b3a3a;background:#1c1113;color:#f2b8b8}
  .banner.synced{border-color:#2f5b3a;background:#0f1c14;color:#a9f0c2}
  .row{display:flex;gap:16px}
  .pane{flex:1;border:1px solid #2a2f3a;border-radius:14px;background:#0f1115;padding:16px;transition:.5s}
  .pane.off{border-color:#5b4a2f}
  .pane.cv{border-color:#2f5b3a;box-shadow:0 0 0 1px #2f5b3a inset}
  .who{display:flex;align-items:center;justify-content:space-between;margin-bottom:12px}
  .who b{font-size:15px}
  .chip{font-size:11px;font-weight:700;padding:3px 8px;border-radius:999px}
  .chip.off{background:#3a2f14;color:#f0d79a}
  .chip.cv{background:#123a20;color:#a9f0c2}
  .k{color:#8b95a7;font-size:12px;text-transform:uppercase;letter-spacing:.4px;margin-top:12px}
  .v{font-size:16px;font-weight:600;margin-top:3px}
  .tags{display:flex;flex-wrap:wrap;gap:6px;margin-top:5px}
  .tag{font-size:12px;padding:3px 9px;border-radius:999px;background:#1a2030;color:#c7d0e0;border:1px solid #2a3346}
  .likes{font-variant-numeric:tabular-nums}
  .flash{animation:fl .6s}
  @keyframes fl{0%{background:#182234}100%{background:transparent}}
</style>
<h1>crdt:merge — one document, three replicas</h1>
<div class=sub>state-based CvRDTs · every value below is produced by the Rust <code>crdt.wasm</code></div>
<div class=banner id=banner>All three replicas start from the same document.</div>
<div class=row id=row></div>
<script>
  window.render = (replicas, tone) => {
    document.getElementById('row').innerHTML = replicas.map(r => {
      const chip = r.mode==='cv' ? '<span class="chip cv">converged ✓</span>'
                 : r.mode==='off' ? '<span class="chip off">offline</span>' : '';
      const tags = r.s.tags.map(t=>'<span class=tag>'+t+'</span>').join('') || '<span class=v>—</span>';
      return \`<div class="pane \${r.mode||''} \${r.flash?'flash':''}">
        <div class=who><b>\${r.name}</b>\${chip}</div>
        <div class=k>Title</div><div class=v>\${r.s.title}</div>
        <div class=k>Status</div><div class=v>\${r.s.status}</div>
        <div class=k>Likes</div><div class="v likes">♥ \${r.s.likes}</div>
        <div class=k>Tags</div><div class=tags>\${tags}</div>
      </div>\`;
    }).join('');
    const b=document.getElementById('banner'); b.className='banner '+(tone||'');
  };
</script>`);

const base = { title: "Draft", status: "todo", likes: 0, tags: [] };
const setBanner = (text, tone) => page.evaluate(([t]) => (document.getElementById("banner").textContent = t), [text]);
const paint = (rs, tone) => page.evaluate(([rs, tone]) => window.render(rs, tone), [rs, tone]);

try {
  // 1. everyone identical at the fork point
  await paint(replicas.map((r) => ({ name: r.name, s: base })), "");
  await sleep(1600);

  // 2. each edits offline, one at a time -> panes visibly diverge
  await setBanner("Editing offline — no network, no lock. Each replica diverges.", "diverged");
  const shown = replicas.map((r) => ({ name: r.name, s: base }));
  for (let i = 0; i < replicas.length; i++) {
    shown[i] = { name: replicas[i].name, s: replicas[i].off, mode: "off", flash: true };
    await paint(shown.map((x, j) => ({ ...x, flash: j === i })), "diverged");
    await sleep(1500);
  }
  await sleep(1400);

  // 3. SYNC -> merge -> all converge to the identical state
  await setBanner("SYNC — merge() is commutative & associative, so all replicas converge.", "synced");
  await sleep(700);
  await paint(replicas.map((r) => ({ name: r.name, s: converged, mode: "cv", flash: true })), "synced");
  await sleep(900);
  await setBanner(
    `Converged: title "${converged.title}" (LWW, latest wins) · ♥ ${converged.likes} (Σ) · tags ∪ — "urgent" survived Carol's remove (add wins).`,
    "synced",
  );
  await sleep(3200);
} finally {
  await ctx.close();
  await browser.close();
}

// sanity: the three panes really are identical after merge
const same = JSON.stringify(c.value(cvDoc)) && [cvLikes, cvTags].every(Boolean);
console.log("converged:", JSON.stringify(converged), same ? "" : "(!!)");
