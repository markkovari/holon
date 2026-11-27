// Ideation-to-Settled Vet-Clinic SDLC Story (~80s, 1520x900)
//
// Shows a concrete, immediately recognizable product change:
//   OLD: Classic 1-Page All-in-One Appointment Form
//   NEW: Guided 3-Step Appointment Wizard (① Pet & Plan -> ② Symptoms -> ③ Care Plan)
//
// Walks through the ENTIRE SDLC from Ideation -> Dev/Gate -> Beta Users First (stg)
// -> Live A/B Testing -> Multi-Rollout Strategies (FIFO / Exclusive Layers / Human Pick)
// -> Guardrail Auto-Rollback -> Gradual Rollout (25% -> 50% -> 100%) -> Settled in Prod!

import { mkdirSync } from "node:fs";
import { chromium } from "playwright";

const BASE = process.env.DELIVERY_URL || "http://127.0.0.1:3090";
const OUT_DIR = new URL("./videos/vet-clinic-wizard-sdlc/", import.meta.url).pathname;
mkdirSync(OUT_DIR, { recursive: true });

const W = 1520;
const H = 900;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function api(method, path, body, headers = {}) {
  const res = await fetch(BASE + path, {
    method,
    headers: { "content-type": "application/json", ...headers },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let json = null;
  try { json = JSON.parse(text); } catch {}
  return { status: res.status, ok: res.ok, json, text };
}

const STUDIO_HTML = `<!doctype html>
<html>
<head>
<meta charset="utf-8" />
<style>
  :root {
    --bg: #05080f; --panel: #0d1320; --panel2: #141d2e; --border: #233047;
    --text: #e6edf7; --muted: #8b9bb4; --blue: #60a5fa; --green: #34d399;
    --amber: #fbbf24; --red: #f87171;
  }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body {
    background: var(--bg); color: var(--text);
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    height: 100vh; display: flex; flex-direction: column; overflow: hidden;
    position: relative;
  }
  .topbar {
    height: 54px; padding: 0 18px; background: linear-gradient(90deg, #0f172a, #1e3a8a);
    border-bottom: 1px solid var(--border);
    display: flex; align-items: center; justify-content: space-between;
  }
  .title-wrap { display: flex; align-items: center; gap: 12px; }
  .logo {
    background: #2563eb; color: #fff; font-weight: 800; font-size: 12px;
    padding: 4px 10px; border-radius: 6px; letter-spacing: 0.06em;
  }
  #scene-title { font-size: 16px; font-weight: 700; color: #fff; }
  .scene-pill {
    font-size: 11.5px; font-weight: 700; padding: 4px 12px; border-radius: 999px;
    background: rgba(52,211,153,0.16); color: var(--green); border: 1px solid rgba(52,211,153,0.4);
  }
  .grid {
    flex: 1; display: grid;
    grid-template-columns: 360px 1fr 445px;
    grid-template-rows: 1fr 1fr;
    gap: 10px; padding: 10px 10px 72px 10px; min-height: 0;
  }
  .pane {
    background: var(--panel); border: 1px solid var(--border); border-radius: 10px;
    display: flex; flex-direction: column; min-height: 0; overflow: hidden;
  }
  .pane-term { grid-column: 1; grid-row: 1 / span 2; }
  .pane-dash { grid-column: 2; grid-row: 1 / span 2; }
  .pane-app1 { grid-column: 3; grid-row: 1; }
  .pane-app2 { grid-column: 3; grid-row: 2; }
  .pane-head {
    padding: 7px 12px; background: var(--panel2); border-bottom: 1px solid var(--border);
    font-size: 11px; font-weight: 700; text-transform: uppercase; letter-spacing: 0.06em;
    color: var(--muted); display: flex; justify-content: space-between; align-items: center;
  }
  .idea-card {
    margin: 10px 10px 4px; padding: 10px 12px; border-radius: 8px;
    background: linear-gradient(135deg, rgba(37,99,235,0.22), rgba(52,211,153,0.12));
    border: 1px solid rgba(96,165,250,0.45); font-size: 12px; line-height: 1.4;
  }
  .idea-card .ih { font-weight: 800; color: #93c5fd; margin-bottom: 4px; font-size: 11.5px; text-transform: uppercase; letter-spacing: 0.05em; }
  .idea-card b { color: #fff; }
  .term {
    flex: 1; padding: 8px 12px; overflow-y: auto;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11.5px; line-height: 1.44;
  }
  .cmd { color: #93c5fd; margin-top: 9px; font-weight: 600; }
  .cmd::before { content: "$ "; color: var(--green); }
  .out { color: #cbd5e1; white-space: pre-wrap; word-break: break-word; margin-top: 2px; }
  .out.ok { color: var(--green); }
  .out.warn { color: var(--amber); }
  .out.err { color: var(--red); }
  iframe { flex: 1; width: 100%; border: 0; background: #0b0f17; }
  iframe.app { background: #f8fafc; }

  /* Cinematic Subtitle Bar */
  #subtitle-wrap {
    position: fixed; left: 0; right: 0; bottom: 12px;
    display: flex; justify-content: center; pointer-events: none; z-index: 9000;
  }
  #subtitle {
    max-width: 1220px; padding: 10px 22px; border-radius: 10px;
    background: rgba(5, 8, 15, 0.92); border: 1px solid rgba(96, 165, 250, 0.5);
    box-shadow: 0 8px 28px rgba(0, 0, 0, 0.7);
    color: #f8fafc; font-size: 15px; font-weight: 500; text-align: center;
    letter-spacing: 0.01em; line-height: 1.35;
  }
  #subtitle b { color: #93c5fd; font-weight: 700; }

  /* Animated Glowing Cursor + Click Ripple */
  #cursor {
    position: fixed; left: 760px; top: 450px; z-index: 9999; pointer-events: none;
    transition: left 0.55s cubic-bezier(0.22, 1, 0.36, 1), top 0.55s cubic-bezier(0.22, 1, 0.36, 1);
    transform: translate(-3px, -3px);
  }
  #cursor-label {
    position: absolute; left: 22px; top: 18px;
    background: #2563eb; color: #fff; font-size: 10.5px; font-weight: 700;
    padding: 2px 8px; border-radius: 999px; white-space: nowrap;
    box-shadow: 0 3px 10px rgba(0,0,0,0.45);
  }
  .ripple {
    position: fixed; width: 38px; height: 38px; border-radius: 50%;
    border: 3px solid #60a5fa; background: rgba(96,165,250,0.22);
    transform: translate(-50%, -50%) scale(0.3); opacity: 1;
    pointer-events: none; z-index: 9998;
    animation: rippleOut 0.55s ease-out forwards;
  }
  @keyframes rippleOut {
    to { transform: translate(-50%, -50%) scale(1.7); opacity: 0; }
  }
</style>
</head>
<body>
  <div class="topbar">
    <div class="title-wrap">
      <span class="logo">MEADOWBROOK VET · FULL SDLC</span>
      <div id="scene-title">From Product Idea to Settled in Production</div>
    </div>
    <span id="scene-pill" class="scene-pill">IDEATION</span>
  </div>
  <div class="grid">
    <div class="pane pane-term">
      <div class="pane-head"><span>Product Idea &amp; Delivery Log</span><span>vet-clinic</span></div>
      <div class="idea-card">
        <div class="ih">💡 Feature Idea · CLINIC-201</div>
        <div><b>Goal:</b> Replace the crowded 1-page appointment form with a <b>Guided 3-Step Booking Wizard</b> (Pet → Symptoms → Care Plan) to lift completed bookings by +15%.</div>
      </div>
      <div id="term" class="term"></div>
    </div>
    <div class="pane pane-dash">
      <div class="pane-head"><span>vet-clinic · Delivery Control</span><span>live updates</span></div>
      <iframe id="dash" src="${BASE}/"></iframe>
    </div>
    <div class="pane pane-app1">
      <div class="pane-head"><span>🐾 Main Clinic · Alice</span><span style="color:#60a5fa">Production Customer</span></div>
      <iframe id="app-alice" class="app" src="${BASE}/apps/pet-clinic?user=Alice&subject=owner-alice&tenant=main-clinic&stage=prod&species=dog&age=9&minutes=30&symptoms=annual+checkup&tier=vip"></iframe>
    </div>
    <div class="pane pane-app2">
      <div class="pane-head"><span>🐾 Pilot Clinic · Dr. Maya</span><span style="color:#a78bfa">Early-Access Beta Vet</span></div>
      <iframe id="app-maya" class="app" src="${BASE}/apps/pet-clinic?user=Dr.+Maya&subject=dr-maya&tenant=pilot-clinic&stage=stg&cohort=beta-vets&species=cat&age=10&minutes=15&symptoms=difficulty+breathing&tier=vip"></iframe>
    </div>
  </div>

  <div id="subtitle-wrap">
    <div id="subtitle"><b>Step 1 · The Idea:</b> Pet owners drop off on our long 1-page booking form. Let's ship a <b>Guided 3-Step Appointment Wizard</b> — safely, from dev all the way to settled production.</div>
  </div>

  <div id="cursor">
    <svg width="26" height="26" viewBox="0 0 24 24" fill="none">
      <path d="M4 3L19 12L12.5 13.8L10 20.5L4 3Z" fill="#ffffff" stroke="#1d4ed8" stroke-width="2" stroke-linejoin="round"/>
    </svg>
    <span id="cursor-label">Product Lead</span>
  </div>
</body>
</html>`;

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({
  viewport: { width: W, height: H },
  recordVideo: { dir: OUT_DIR, size: { width: W, height: H } },
});
const page = await ctx.newPage();

await page.setContent(STUDIO_HTML);
const dash = () => page.frameLocator("#dash");
const aliceApp = () => page.frameLocator("#app-alice");
const mayaApp = () => page.frameLocator("#app-maya");

const setSubtitle = async (html, title, pill) => {
  await page.evaluate(
    ([h, t, p]) => {
      document.getElementById("subtitle").innerHTML = h;
      if (t) document.getElementById("scene-title").textContent = t;
      if (p) document.getElementById("scene-pill").textContent = p;
    },
    [html, title, pill],
  );
};

const moveCursor = async (x, y, label = "Director") => {
  await page.evaluate(
    ([cx, cy, lbl]) => {
      const c = document.getElementById("cursor");
      c.style.left = Math.round(cx) + "px";
      c.style.top = Math.round(cy) + "px";
      document.getElementById("cursor-label").textContent = lbl;
    },
    [x, y, label],
  );
  await sleep(600);
};

const rippleAt = async (x, y) => {
  await page.evaluate(
    ([cx, cy]) => {
      const r = document.createElement("div");
      r.className = "ripple";
      r.style.left = Math.round(cx) + "px";
      r.style.top = Math.round(cy) + "px";
      document.body.appendChild(r);
      setTimeout(() => r.remove(), 650);
    },
    [x, y],
  );
};

const cursorClick = async (frameLocator, selector, label = "Director") => {
  const loc = frameLocator.locator(selector).first();
  await loc.scrollIntoViewIfNeeded();
  const box = await loc.boundingBox();
  if (box) {
    const cx = box.x + box.width / 2;
    const cy = box.y + box.height / 2;
    await moveCursor(cx, cy, label);
    await rippleAt(cx, cy);
    await sleep(180);
  }
  await loc.click();
};

const cursorSelect = async (frameLocator, selector, value, label = "Director") => {
  const loc = frameLocator.locator(selector).first();
  await loc.scrollIntoViewIfNeeded();
  const box = await loc.boundingBox();
  if (box) {
    const cx = box.x + box.width / 2;
    const cy = box.y + box.height / 2;
    await moveCursor(cx, cy, label);
    await rippleAt(cx, cy);
    await sleep(180);
  }
  await loc.selectOption(value);
};

const logCli = async (cmd, out, cls = "") => {
  await page.evaluate(
    ([c, o, cl]) => {
      const el = document.getElementById("term");
      if (c) {
        const d = document.createElement("div");
        d.className = "cmd";
        d.textContent = c;
        el.appendChild(d);
      }
      if (o) {
        const d = document.createElement("div");
        d.className = "out " + cl;
        d.textContent = o;
        el.appendChild(d);
      }
      el.scrollTop = el.scrollHeight;
    },
    [cmd, out, cls],
  );
};

try {
  await dash().locator("#sse-status").filter({ hasText: "SSE Live" }).waitFor({ timeout: 10000 });
  await moveCursor(180, 145, "Product Lead");
  await sleep(2800);

  // =========================================================================
  // CHAPTER 1: IDEATION -> DRAFT -> DEV & AUTOMATED QUALITY GATE
  // =========================================================================
  await setSubtitle(
    "<b>Dev &amp; Quality Gate:</b> Today both Alice and Dr. Maya see the <b>Classic 1-Page Form</b>. Engineering registers the new <b>3-Step Appointment Wizard</b> in <b>draft</b> and runs automated checks.",
    "1 · Ideation → Draft → Automated Quality Gate",
    "DRAFT → GATED",
  );
  await logCli(
    "clinic deliver create guided-3step-booking-wizard --stage draft",
    "Registered in vet-clinic (draft) alongside urgent-lab-billing-sync",
  );
  const petRes = await api("POST", "/api/deliverables", {
    project: "vet-clinic",
    name: "guided-3step-booking-wizard",
    capability: "clinic:triage/evaluator@0.1.0",
    goal_ref: "CLINIC-201: 3-Step Booking Wizard",
    stage: "draft",
    control: {
      id: "classic-1page-form",
      role: "control",
      language: "standard",
      digest: "sha256:2c132ffa860b3a0eea017fff1690749eb33c55482bb66d4a32ff3da3c1097b56",
      weight: 50,
    },
    candidate: {
      id: "guided-3step-wizard",
      role: "candidate",
      language: "candidate",
      digest: "sha256:a4e647017699ccafa3642196f5e84d2a1b9c8e7f6a5b4c3d2e1f0a9b8c7d6e5f",
      weight: 50,
      strategy_params: {
        vip_discount_pct: 25,
        senior_age: 7,
        avian_specialist: "dr-feather-wing",
      },
    },
    wiring: {
      stg_tenants: ["pilot-clinic"],
      stg_cohorts: ["beta-vets"],
      allow_stage_header: true,
      rollout_steps: [25, 50, 100],
    },
    guardrails: { max_error_rate: 0.1, min_exposures: 4, min_lift: 0.05 },
  });
  const PET_ID = petRes.json.id;

  const billRes = await api("POST", "/api/deliverables", {
    project: "vet-clinic",
    name: "urgent-lab-billing-sync",
    capability: "clinic:billing/invoice@0.1.0",
    goal_ref: "CLINIC-98: Auto-Invoice Lab Panels",
    stage: "rollout",
    control: { id: "manual-lab-invoice", role: "control", language: "standard", digest: "sha256:1111aaaa", weight: 50 },
    candidate: { id: "auto-lab-invoice", role: "candidate", language: "candidate", digest: "sha256:2222bbbb", weight: 50 },
    wiring: { rollout_pct: 50, rollout_steps: [25, 50, 100] },
    guardrails: { max_error_rate: 0.1, min_exposures: 5, min_lift: 0.0 },
  });
  const BILL_ID = billRes.json.id;
  await sleep(1600);

  await logCli("clinic deliver gate guided-3step-booking-wizard", "UI + pricing + clinical checks PASSED -> stage: gated", "ok");
  await api("POST", `/api/deliverables/${PET_ID}/stage`, { stage: "gated", reason: "all checks green" });
  await sleep(1800);

  // =========================================================================
  // CHAPTER 2: FOR SOME USERS FIRST (Parallel Staging: Pilot Clinic vs Main Clinic)
  // =========================================================================
  await setSubtitle(
    "<b>Step 2 · Release to Some Users First (Staging):</b> We turn on the <b>3-Step Wizard</b> for <b>Pilot Clinic (Dr. Maya)</b> only — while <b>Main Clinic (Alice)</b> stays on the 1-Page Form.",
    "2 · Early Access for Pilot Users Only (Parallel Staging)",
    "STAGE: STAGING",
  );
  await logCli("clinic deliver release-stg guided-3step-booking-wizard", "Enabled ONLY for pilot-clinic & beta-vets", "ok");
  await api("POST", `/api/deliverables/${PET_ID}/stage`, { stage: "stg", reason: "pilot clinic early access" });
  await sleep(1500);

  // Alice refreshes & checks — still Classic 1-Page Form!
  await setSubtitle(
    "<b>Alice (Main Clinic)</b> uses the app — she still sees the <b>Classic 1-Page Form</b> ($75.00, Dr. Sarah Lin). Zero disruption to production customers.",
    "2 · Main Clinic (Alice) Stays on Classic 1-Page Form",
    "PROD SAFE",
  );
  await cursorClick(aliceApp(), "#btn-sync", "Alice (Prod)");
  await sleep(700);
  await cursorClick(aliceApp(), "#btn-classic-check", "Alice (Prod)");
  await sleep(2600);

  // Dr. Maya refreshes — INSTANTLY gets the brand-new Guided 3-Step Wizard!
  await setSubtitle(
    "Now <b>Dr. Maya (Pilot Clinic)</b> refreshes — her app switches to the <b>★ NEW 3-Step Wizard</b>! Watch her step through <b>① Pet &amp; Plan → ② Symptoms → ③ Care Plan &amp; Confirm</b>.",
    "2 · Pilot Clinic (Dr. Maya) Works Through the New 3-Step Wizard!",
    "EARLY ACCESS",
  );
  await cursorClick(mayaApp(), "#btn-sync", "Dr. Maya (Pilot)");
  await sleep(1400);
  await cursorClick(mayaApp(), "#btn-wiz-next1", "Dr. Maya (Pilot)");
  await sleep(1400);
  await cursorClick(mayaApp(), "#btn-wiz-next2", "Dr. Maya (Pilot)");
  await sleep(2200);
  await cursorClick(mayaApp(), "#btn-confirm", "Dr. Maya (Pilot)");
  await sleep(2400);

  // =========================================================================
  // CHAPTER 3: LIVE A/B TESTING (Measure Real Booking Conversion Lift)
  // =========================================================================
  await setSubtitle(
    "<b>Step 3 · Live A/B Testing:</b> Pilot feedback is great! We open a <b>50/50 A/B test</b> on Main Clinic so half of pet owners get the 1-Page Form and half get the 3-Step Wizard.",
    "3 · Live 50/50 A/B Testing & Real-Time Conversion Lift",
    "STAGE: A/B TEST",
  );
  await logCli("clinic deliver start-ab guided-3step-booking-wizard", "50/50 A/B experiment live on Main Clinic", "ok");
  await api("POST", `/api/deliverables/${PET_ID}/stage`, { stage: "ab_test", reason: "pilot clinic approved" });
  await sleep(1000);

  await dash().locator("#detail-panel").first().scrollIntoViewIfNeeded();
  await moveCursor(650, 500, "Live A/B");
  await sleep(900);

  let cCtrl = 0;
  let cCand = 0;
  for (let i = 1; i <= 8; i++) {
    const r = await api("POST", `/api/deliverables/${PET_ID}/triage`, {
      context: { tenant: "main-clinic", subject: `owner-${i}`, cohort: "general", stage: "prod" },
      species: "dog",
      age_years: 8,
      minutes: 30,
      symptoms: "annual checkup",
      owner_tier: "vip",
    });
    const role = r.json._meta.wiring.role;
    if (role === "control") {
      cCtrl++;
      if (cCtrl === 1) {
        await api("POST", `/api/deliverables/${PET_ID}/outcome`, { variant_id: r.json.variant_id, kind: "converted" });
      }
    } else {
      cCand++;
      if (cCand <= 4) {
        await api("POST", `/api/deliverables/${PET_ID}/outcome`, { variant_id: r.json.variant_id, kind: "converted" });
      }
    }
    await sleep(380);
  }
  await logCli("A/B Results (8 Main Clinic owners)", `1-Page Form=${cCtrl} vs 3-Step Wizard=${cCand} · Huge booking lift!`, "ok");
  await sleep(2800);

  // =========================================================================
  // CHAPTER 4: MULTIPLE FEATURES COMPETING (FIFO -> Exclusive Layers -> Human Pick)
  // =========================================================================
  await setSubtitle(
    "<b>Step 4 · Multiple Rollouts at Once:</b> A second feature (<b>Annual Dental Add-On</b>) wants to test on the same booking screen. Under <b>FIFO</b>, it is safely queued (HTTP 409).",
    "4 · Multi-Rollout Strategies (FIFO · Exclusive Layers · Human Pick)",
    "STRATEGY: FIFO",
  );
  const d2 = await api("POST", "/api/deliverables", {
    project: "vet-clinic",
    name: "annual-dental-addon",
    capability: "clinic:triage/evaluator@0.1.0",
    goal_ref: "CLINIC-208: Dental Add-On",
    stage: "stg",
  });
  await sleep(1000);
  const blocked = await api("POST", `/api/deliverables/${d2.json.id}/stage`, { stage: "ab_test" });
  await moveCursor(180, 560, "Policy Guard");
  await logCli("clinic deliver start-ab annual-dental-addon", `HTTP ${blocked.status}: ${blocked.json.error}`, "err");
  await sleep(2600);

  await setSubtitle(
    "Want to run both experiments safely? Switch to <b>Mutually Exclusive Layers</b> (disjoint customer slices <b>1/2</b> &amp; <b>2/2</b>) or <b>Human Pick</b> (Director approval gate).",
    "4 · Switching Strategies Live on the Dashboard",
    "STRATEGIES",
  );
  await cursorSelect(dash(), "#strategy-select", "mutually_exclusive_layers", "Director");
  await logCli("Strategy -> mutually_exclusive_layers", "Disjoint customer slices (1/2, 2/2)", "warn");
  await sleep(2600);
  await cursorSelect(dash(), "#strategy-select", "human_pick", "Director");
  await logCli("Strategy -> human_pick", "Held until Director clicks Pick for Release", "warn");
  await sleep(2400);
  await cursorSelect(dash(), "#strategy-select", "fifo", "Director");
  await dash().locator("#portfolio-kpis").first().scrollIntoViewIfNeeded();
  await sleep(900);

  // =========================================================================
  // CHAPTER 5: GUARDRAIL SAFETY NET + GRADUAL ROLLOUT (25% -> 50% -> 100%)
  // =========================================================================
  await setSubtitle(
    "<b>Step 5 · Guardrail Safety &amp; Gradual Rollout:</b> When <b>Urgent Lab Billing Sync</b> hits a 20% error spike, it <b>auto-rolls back instantly</b> — while the healthy <b>3-Step Wizard</b> steps up <b>25% → 50% → 100%</b>!",
    "5 · Inline Auto-Rollback & Gradual Rollout to 100%",
    "ROLLOUT → 100%",
  );
  for (let i = 0; i < 5; i++) {
    await api("POST", `/api/deliverables/${BILL_ID}/outcome`, { variant_id: "auto-lab-invoice", kind: "exposed" });
  }
  const rb = await api("POST", `/api/deliverables/${BILL_ID}/outcome`, { variant_id: "auto-lab-invoice", kind: "error" });
  await logCli("urgent-lab-billing-sync error spike (20%)", `INLINE auto_rolled_back=${rb.json.auto_rolled_back}`, "err");
  await sleep(2400);

  for (let t = 1; t <= 3; t++) {
    const tick = await api("POST", "/api/tick");
    const act = tick.json.actions.find((a) => a.deliverable_id === PET_ID);
    await logCli(`Gradual Rollout Judge (#${t})`, `${act.verdict.action} -> ${act.verdict.next_stage} (${act.verdict.next_rollout_pct}%)`, "ok");
    await sleep(1600);
  }

  // =========================================================================
  // CHAPTER 6: SETTLED IN PRODUCTION — ALICE'S APP TRANSFORMS LIVE!
  // =========================================================================
  await setSubtitle(
    "<b>Step 6 · Settled in Production (100%)!</b> <b>Alice</b> refreshes Main Clinic — her 1-Page Form is now the <b>★ NEW 3-Step Wizard</b>! Watch her book with <b>Dr. Elena Vance &amp; 25% PawClub Savings ($56.25)</b>.",
    "6 · Feature Settled at 100% — Alice Uses the New 3-Step Wizard!",
    "SETTLED · 100%",
  );
  await cursorClick(aliceApp(), "#btn-sync", "Alice (Prod)");
  await sleep(1500);
  await cursorClick(aliceApp(), "#btn-wiz-next1", "Alice (Prod)");
  await sleep(1400);
  await cursorClick(aliceApp(), "#btn-wiz-next2", "Alice (Prod)");
  await sleep(2200);
  await cursorClick(aliceApp(), "#btn-confirm", "Alice (Prod)");
  await sleep(3600);
} finally {
  const videoPath = await page.video().path();
  await ctx.close();
  await browser.close();
  console.log("RECORDED_VIDEO=" + videoPath);
}
