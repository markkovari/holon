// Where the console breaks.
//
// The happy path passes; this is the suite written to make it fail. Each test
// aims at something the implementation does not obviously handle, and the ones
// that pass are only interesting because they were written to fail.
//
// Anything that goes red here is a finding, not a broken test — fix the console,
// not the assertion, unless the assertion is wrong about what the product should
// do (which happened once already: the run view is behind the login, and the test
// that assumed otherwise was the thing that was wrong).

import { expect, type Page, test } from "@playwright/test";

const SURREAL = "http://127.0.0.1:8111";

/// Write straight to the store, bypassing the seeder.
///
/// The seeder writes a WELL-FORMED run through `trace.rs`. Half the point here is
/// the malformed and the extreme, which the writer would never emit — so these go
/// in directly.
async function surql(page: Page, body: string) {
  const r = await page.request.post(`${SURREAL}/sql`, {
    headers: { accept: "application/json", "surreal-ns": "comp", "surreal-db": "goalmemory" },
    data: body,
  });
  expect(r.ok(), `seeding failed: ${await r.text()}`).toBeTruthy();
  return r.json();
}

async function signIn(page: Page) {
  await page.goto("/");
  await page.getByPlaceholder("email").fill("e2e@example.test");
  await page.getByPlaceholder("password").fill("hunter2");
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByTestId("tab-runs")).toBeVisible();
}

test.describe("the console under pressure", () => {
  test("a goal containing markup is rendered as text, not as markup", async ({ page }) => {
    // The console's whole security posture rests on the page rendering
    // model-written and human-written prose safely — it is why the session is an
    // HttpOnly cookie in the first place. A goal is the most attacker-adjacent
    // string here: a person types it, and a model reads it.
    const xss = `<img src=x onerror="window.__pwned=1">`;
    await surql(
      page,
      `UPSERT run:⟨xss/g1⟩ SET id_text = 'xss/g1', goal = ${JSON.stringify(xss)}, outcome = 'merged', started_at = time::now();`,
    );

    await signIn(page);
    await page.getByTestId("tab-runs").click();
    await expect(page.getByTestId("run-xss/g1")).toBeVisible();

    // The markup must be visible AS TEXT and must not have executed.
    await expect(page.getByTestId("run-list")).toContainText("onerror");
    expect(await page.evaluate(() => (window as any).__pwned)).toBeUndefined();
    expect(await page.locator("img[src='x']").count()).toBe(0);
  });

  test("an unresolved run renders instead of showing nothing", async ({ page }) => {
    // A RUNNING run has no outcome, no winner, no resolved_at. That is the state
    // a person is most likely to be looking at — the run they just started — and
    // it is the one the seeder never produces.
    await surql(
      page,
      `UPSERT run:⟨live/g1⟩ SET id_text = 'live/g1', goal = 'a run still going', branches = 4, started_at = time::now(), resolved_at = NONE;`,
    );

    await signIn(page);
    await page.getByTestId("tab-runs").click();
    await page.getByTestId("run-live/g1").click();

    await expect(page.getByTestId("run-detail")).toBeVisible();
    await expect(page.getByTestId("run-goal")).toHaveText("a run still going");
    // Not blank, not "undefined" — the view has a word for "not finished yet".
    await expect(page.getByTestId("run-outcome")).toHaveText("running");
    await expect(page.getByTestId("run-detail")).not.toContainText("undefined");
  });

  test("an event kind the view has never seen does not break the timeline", async ({ page }) => {
    // ADR-0092 says the vocabulary is append-only and cheap to extend. The test
    // of that claim is a console built before the new kind existed: it must keep
    // rendering the events around it.
    await surql(
      page,
      `UPSERT run:⟨future/g1⟩ SET id_text = 'future/g1', goal = 'from a newer driver', outcome = 'merged', started_at = time::now();
       CREATE event SET run = 'future/g1', kind = 'run-started', data = { seed: 1 }, at = time::now();
       CREATE event SET run = 'future/g1', kind = 'quantum-entangled', data = { spooky: true }, at = time::now();
       CREATE event SET run = 'future/g1', kind = 'run-resolved', data = { outcome: 'merged' }, at = time::now();`,
    );

    await signIn(page);
    await page.getByTestId("tab-runs").click();
    await page.getByTestId("run-future/g1").click();

    const timeline = page.getByTestId("timeline");
    await expect(timeline).toContainText("run-started");
    await expect(timeline).toContainText("quantum-entangled");
    // The known ones either side still render — an unknown kind must not
    // truncate the timeline.
    await expect(timeline).toContainText("run-resolved");
  });

  test("an event with no data at all does not blank the page", async ({ page }) => {
    // `data` is written by the driver and could be absent from a partial write,
    // an older schema, or a hand-edited row. `describe()` reaches into it.
    await surql(
      page,
      `UPSERT run:⟨sparse/g1⟩ SET id_text = 'sparse/g1', goal = 'a sparse run', outcome = 'failed', started_at = time::now();
       CREATE event SET run = 'sparse/g1', kind = 'gate-verdict', at = time::now();`,
    );

    await signIn(page);
    await page.getByTestId("tab-runs").click();
    await page.getByTestId("run-sparse/g1").click();

    await expect(page.getByTestId("run-detail")).toBeVisible();
    await expect(page.getByTestId("timeline")).toContainText("gate-verdict");
  });

  test("a run id with unicode and spaces survives the round trip", async ({ page }) => {
    // Run ids come from `run_id(seed, round, branch)` and a branch name is a
    // model-chosen string. The id is bracket-quoted in SurrealQL and
    // percent-encoded in the URL, and both have to hold.
    const id = "99/g1/naïve approach ✨";
    await surql(
      page,
      `UPSERT run:⟨${id}⟩ SET id_text = ${JSON.stringify(id)}, goal = 'unicode branch', outcome = 'merged', started_at = time::now();`,
    );

    await signIn(page);
    await page.getByTestId("tab-runs").click();
    await page.getByTestId(`run-${id}`).click();
    await expect(page.getByTestId("run-goal")).toHaveText("unicode branch");
  });

  test("the run list is bounded, and says so by not growing forever", async ({ page }) => {
    // `runs()` caps at 50 deliberately. A store with more must not return more —
    // an unbounded list here is the query that gets slow silently.
    const many = Array.from({ length: 60 }, (_, i) =>
      `UPSERT run:⟨bulk-${i}⟩ SET id_text = 'bulk-${i}', goal = 'bulk ${i}', outcome = 'merged', started_at = time::now();`,
    ).join("\n");
    await surql(page, many);

    try {
      await signIn(page);
      const answer = await (await page.request.get("/api/runs")).json();
      expect(answer.runs.length).toBeLessThanOrEqual(50);
    } finally {
      // Clean up, because the store is shared with every other spec and the cap
      // is real: sixty extra runs push the seeded one off the list and fail a
      // test that is about something else entirely. A suite that leaves bulk
      // data behind turns one finding into an unrelated red.
      await surql(page, "DELETE run WHERE string::starts_with(id_text, 'bulk-');");
    }
  });

  test("a session that stops being valid returns to the login form", async ({ page }) => {
    // Tokens expire. The console must notice on the next call rather than
    // rendering an empty shell forever.
    await signIn(page);
    await page.context().clearCookies();
    await page.reload();
    await expect(page.getByPlaceholder("email")).toBeVisible();
  });

  test("a run that does not exist is not-found, not a crash", async ({ page }) => {
    await signIn(page);
    const answer = await page.request.get("/api/runs/no-such-run");
    expect(answer.ok()).toBeTruthy();
    expect((await answer.json()).run).toBeNull();
  });
});
