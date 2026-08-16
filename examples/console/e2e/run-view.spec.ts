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

/// Sign in through the form, not by planting a cookie.
///
/// The session is an HttpOnly cookie the console sets after forwarding the login
/// to the platform, so a test that fabricated one would skip the exchange that
/// makes every later request authenticate. Driving the form is also the only way
/// to prove the SPA can actually get in.
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

    // Both branches, with the loser kept, and what each one actually DID.
    // "Why did branch 3 beat branch 7" is unanswerable if the failures are
    // dropped, and only half-answerable if their output is.
    const attempts = page.getByTestId("attempts");
    await expect(attempts).toContainText("risk-first");
    await expect(attempts).toContainText("mvp");
    await expect(attempts).toContainText("40");
    await expect(attempts).toContainText("100");

    // The two branches took different approaches, and the page shows it: one
    // edited the app, the other extracted a component. That difference is the
    // reason to run more than one branch at all.
    await expect(attempts).toContainText("apps/search/src/query.rs");
    await expect(attempts).toContainText("components/paginate/src/lib.rs");
    // Cost and duration, which exist nowhere else once the terminal is gone.
    await expect(attempts).toContainText("31.2k tok");

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
