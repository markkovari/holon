// The run view, in a real browser, against the real stack (ADR-0092).
//
//   playwright → console-domain (wasm) → knowledge:graph (wasm) → SurrealDB
//
// Nothing is stubbed below the browser. The store is seeded by `comp-trace-seed`,
// which calls the same `trace.rs` a run calls — so this asserts the shape the
// DRIVER writes, not a fixture somebody hand-wrote to match the UI. A fixture
// would keep passing after the writer's schema drifted, which is the failure this
// test exists to prevent.
//
// The rest of the harness (`globalSetup`) starts SurrealDB and `comp-host`, and
// skips loudly if it cannot — a green run that talked to nothing is worse than a
// red one.

import { expect, type Page, test } from "@playwright/test";

const SURREAL = "http://127.0.0.1:8111";

/// Write straight to the store, so a test can make something happen WHILE the
/// page is open. `comp-trace-seed` runs once in `globalSetup` and cannot.
async function surql(page: Page, body: string) {
  const r = await page.request.post(`${SURREAL}/sql`, {
    headers: { accept: "application/json", "surreal-ns": "comp", "surreal-db": "goalmemory" },
    data: body,
  });
  expect(r.ok(), `seeding failed: ${await r.text()}`).toBeTruthy();
}

/// Sign in through the form, not by planting a cookie.
///
/// The session is an HttpOnly cookie the console sets after forwarding the login
/// to the platform, so a test that fabricated one would skip the exchange that
/// makes every later request authenticate. Driving the form is also the only way
/// to prove the SPA can actually get in.
/// A node on the canvas, by its label.
///
/// Scoped to `.react-flow__node` rather than to the graph box: the selection panel
/// is laid over the canvas and repeats the branch name, so a plain text lookup
/// inside the box matches twice once something is selected.
function graphNode(page: Page, label: string) {
  return page
    .locator(".react-flow__node")
    .filter({ has: page.getByText(label, { exact: true }) })
    .first();
}

async function signIn(page: Page) {
  await page.goto("/");
  await page.getByPlaceholder("email").fill("e2e@example.test");
  await page.getByPlaceholder("password").fill("hunter2");
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByTestId("tab-runs")).toBeVisible();
}

test.describe("the run view", () => {
  test("a run's history survives the terminal closing", async ({ page }) => {
    await signIn(page);

    // The run view reads the knowledge store directly — ADR-0091 keeps run
    // history out of the control plane — but it is still behind the login, like
    // everything else here. Run history is operational data, not something an
    // anonymous visitor gets.
    await page.getByTestId("tab-runs").click();

    const list = page.getByTestId("run-list");
    await expect(list).toBeVisible();

    // The seeded run, by its goal rather than its id: the id is an internal
    // handle and the goal is what a person scans for.
    await expect(list).toContainText("add pagination to the search box");

    await page.getByTestId("run-77/g1").click();

    const detail = page.getByTestId("run-detail");
    await expect(detail).toBeVisible();
    await expect(page.getByTestId("run-goal")).toHaveText("add pagination to the search box");

    // The outcome and winner come from the run NODE, not from scanning events.
    // A timeline that reconstructs its own bounds gets them wrong exactly when
    // an event is missing, which is when you are looking at this page.
    await expect(page.getByTestId("run-outcome")).toHaveText("merged");
    await expect(page.getByTestId("run-winner")).toContainText("mvp");

    // What the pool GAINED (ADR-0089). The only part of a run that outlives the
    // pull request: the app change lands and is done, the component is there for
    // every run after this one.
    await expect(page.getByTestId("capabilities")).toContainText("paginate");
    await expect(page.getByTestId("capabilities")).toContainText("components/paginate");

    // Both branches, with the loser kept. "Why did branch 3 beat branch 7" is
    // unanswerable if the failures are dropped.
    const graph = page.getByTestId("run-graph");
    await expect(graph).toContainText("risk-first");
    await expect(graph).toContainText("mvp");
    await expect(graph).toContainText("40");
    await expect(graph).toContainText("100");

    // The ROUND is on the page, which the flat list could not say. Two branches
    // in one round is a fan-out; two branches in two rounds is a retry, and those
    // are different things that used to render identically.
    await expect(graph).toContainText("round 1");
    await expect(page.getByTestId("run-size")).toContainText("round");

    // The capability hangs off the graph too, not just the banner: it is the last
    // node in `run → round → attempt → capability`.
    await expect(graph).toContainText("paginate");

    // The timeline, in the vocabulary ADR-0092 defines.
    const timeline = page.getByTestId("timeline");
    await expect(timeline).toContainText("run-started");
    await expect(timeline).toContainText("gate-verdict");
    await expect(timeline).toContainText("run-resolved");
    // Described, not dumped as JSON — the fallback is for kinds a newer driver
    // invented, not for ones this view is supposed to know.
    await expect(timeline).toContainText("paginate — the pool can do this now");
    await expect(timeline).not.toContainText('{"name"');

    // The most actionable row on the page: the graph naming a capability the
    // pool lacks (ADR-0089). If this renders as raw JSON the vocabulary has
    // grown a type the view does not know about.
    await expect(timeline).toContainText("nothing for “render a swimlane chart”");
    await expect(timeline).toContainText("the pool is missing this");

    // Back, and the list still stands.
    await page.getByText("← all runs").click();
    await expect(page.getByTestId("run-list")).toBeVisible();
  });

  test("clicking a branch opens its detail and highlights its rows", async ({ page }) => {
    await signIn(page);
    await page.getByTestId("tab-runs").click();
    await page.getByTestId("run-77/g1").click();

    // Nothing is selected until something is clicked. The graph is the shape; the
    // detail is on demand.
    await expect(page.getByTestId("attempt-panel")).toHaveCount(0);

    // The WINNER, because it is the branch whose paths became the capability —
    // the one place `run → round → attempt → capability` is a real chain rather
    // than a diagram.
    await graphNode(page, "mvp").click();

    const panel = page.getByTestId("attempt-panel");
    await expect(panel).toBeVisible();
    await expect(page.getByTestId("panel-branch")).toHaveText("mvp");
    // What the flat list used to carry, now behind one click: the paths and the
    // cost, which exist nowhere else once the terminal is gone.
    await expect(page.getByTestId("panel-paths")).toContainText("components/paginate/src/lib.rs");
    await expect(panel).toContainText("31.2k");
    // Its own events, not the run's.
    await expect(page.getByTestId("panel-events")).toContainText("gate-verdict");
    await expect(page.getByTestId("panel-events")).not.toContainText("run-started");

    // The timeline is HIGHLIGHTED, not filtered. Filtering would destroy the
    // interleaving, and the interleaving is the only thing here that shows two
    // branches running concurrently rather than one after the other — so the
    // other branch's rows and the run-level rows must both still be present.
    const timeline = page.getByTestId("timeline");
    await expect(timeline).toContainText("run-started");
    const lit = timeline.locator("li[data-selected]");
    await expect(lit.first()).toBeVisible();
    const all = await timeline.locator("li").count();
    expect(await lit.count(), "the selection filtered the timeline instead of marking it")
      .toBeLessThan(all);

    // Clicking the same node again clears it.
    await graphNode(page, "mvp").click();
    await expect(page.getByTestId("attempt-panel")).toHaveCount(0);
    await expect(timeline.locator("li[data-selected]")).toHaveCount(0);
  });

  test("an open run picks up what happens next, and the tail after it resolves", async ({
    page,
  }) => {
    // The claim polling exists to make true: a run you are WATCHING updates. And
    // the one that is easy to get wrong — the writes that land after the run says
    // it is finished. `trace.rs` writes the resolution, the attempts and the
    // events as separate statements and counts drops rather than retrying, so
    // stopping the instant `resolved_at` appears truncates the timeline exactly
    // at the end, which is the part somebody opened the page for.
    const RUN = "poll/g1";
    try {
      await surql(
        page,
        `UPSERT run:⟨${RUN}⟩ SET id_text = '${RUN}', goal = 'a run being watched', branches = 2, started_at = time::now(), resolved_at = NONE;
         UPSERT attempt:⟨${RUN}/first⟩ SET id_text = '${RUN}/first', run = '${RUN}', branch = 'first', round = 1, started_at = time::now();`,
      );

      await signIn(page);
      await page.getByTestId("tab-runs").click();
      await page.getByTestId(`run-${RUN}`).click();
      await expect(page.getByTestId("run-graph")).toContainText("first");
      await expect(page.getByTestId("run-graph")).not.toContainText("second");

      // A branch spawns while the page is open. No reload below this line.
      await surql(
        page,
        `UPSERT attempt:⟨${RUN}/second⟩ SET id_text = '${RUN}/second', run = '${RUN}', branch = 'second', round = 1, started_at = time::now();`,
      );
      await expect(page.getByTestId("run-graph")).toContainText("second", { timeout: 15_000 });

      // The run resolves, and then a straggler lands. Both must show up.
      await surql(
        page,
        `UPDATE run:⟨${RUN}⟩ SET outcome = 'merged', winner = 'second', resolved_at = time::now();`,
      );
      await expect(page.getByTestId("run-outcome")).toHaveText("merged", { timeout: 15_000 });
      await surql(
        page,
        `CREATE event SET run = '${RUN}', kind = 'run-resolved', data = { outcome: 'merged' }, at = time::now();`,
      );
      await expect(page.getByTestId("timeline")).toContainText("run-resolved", { timeout: 15_000 });
    } finally {
      await surql(
        page,
        `DELETE event WHERE run = '${RUN}'; DELETE attempt WHERE run = '${RUN}'; DELETE run WHERE id_text = '${RUN}';`,
      );
    }
  });

  test("a run id from the URL cannot carry SurrealQL", async ({ page }) => {
    await signIn(page);
    // The id reaches `run_detail` straight off the path and is interpolated into
    // a statement. This is the assertion that keeps that safe: a hostile id must
    // come back as no-such-run, and the event log must still be there afterwards.
    const hostile = encodeURIComponent(`77/g1'; DELETE event; --`);
    const answer = await page.request.get(`/api/runs/${hostile}`);
    expect(answer.ok()).toBeTruthy();
    const body = await answer.json();
    expect(body.run).toBeNull();

    // The log survived — if the injection had run, the timeline would be empty.
    const still = await (await page.request.get("/api/runs/77%2Fg1")).json();
    expect(still.events.length).toBeGreaterThan(5);
  });
});
