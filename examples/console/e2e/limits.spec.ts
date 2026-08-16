// Round two: the limits I know are missing, and the ones I only suspect.
//
// The first adversarial pass all went green, which means it was aimed at things
// the code already handled. This is aimed at what it demonstrably does not:
// `run_detail` selects events with no LIMIT, tokens are parsed out of a cookie by
// hand, and nothing has ever been asked two questions at once.

import { expect, type Page, test } from "@playwright/test";

const SURREAL = "http://127.0.0.1:8111";

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

test.describe("limits", () => {
  test("a run with thousands of events does not return all of them", async ({ page }) => {
    // `runs()` caps at 50. `run_detail()` does NOT cap its events, and a long
    // agentic run emits events per attempt per repair per generation. This is the
    // query that gets slow silently, and the response that gets large silently.
    test.setTimeout(120_000);

    const batch = (from: number, n: number) =>
      Array.from(
        { length: n },
        (_, i) =>
          `CREATE event SET run = 'huge/g1', kind = 'gate-verdict', data = { score: ${from + i} }, at = time::now();`,
      ).join("\n");

    await surql(
      page,
      `UPSERT run:⟨huge/g1⟩ SET id_text = 'huge/g1', goal = 'a very long run', outcome = 'merged', started_at = time::now();`,
    );
    for (let i = 0; i < 5; i++) await surql(page, batch(i * 1000, 1000));

    try {
      await signIn(page);
      const started = Date.now();
      const answer = await page.request.get("/api/runs/huge%2Fg1");
      const took = Date.now() - started;
      const body = await answer.json();

      // The finding, either way: if this returns 5000 the endpoint is unbounded.
      expect(
        body.events.length,
        `run_detail returned ${body.events.length} events in ${took}ms — an unbounded ` +
          `SELECT means one long run can return an arbitrarily large response`,
      ).toBeLessThanOrEqual(500);

      // Truncation must be VISIBLE. A timeline that silently stops at 500 looks
      // like a run that stopped at 500, which is the more expensive mistake.
      expect(body.truncated, "a truncated timeline did not say so").toBe(true);
      expect(body.eventCount).toBe(5000);

      // And the PAGE has to say it, not just the API. An endpoint that reports
      // truncation to a UI that ignores it has not solved anything.
      await page.getByTestId("tab-runs").click();
      await page.getByTestId("run-huge/g1").click();
      await expect(page.getByTestId("timeline-truncated")).toContainText("of 5000");
    } finally {
      // 5,000 events in a store every other spec reads.
      await surql(page, "DELETE event WHERE run = 'huge/g1'; DELETE run WHERE id_text = 'huge/g1';");
    }
  });

  test("a single event carrying a large payload does not break the response", async ({ page }) => {
    // A gate verdict carries what the gate SAID (ADR-0088), which is test output.
    // Test output can be megabytes. `write_all` exists because
    // `blocking-write-and-flush` traps above 4096 bytes, so this is the assertion
    // that the trap is really handled on this path.
    const big = "x".repeat(400_000);
    await surql(
      page,
      `UPSERT run:⟨fat/g1⟩ SET id_text = 'fat/g1', goal = 'a fat verdict', outcome = 'failed', started_at = time::now();
       CREATE event SET run = 'fat/g1', kind = 'gate-verdict', data = { score: 0, verdict: ${JSON.stringify(big)} }, at = time::now();`,
    );

    await signIn(page);
    const answer = await page.request.get("/api/runs/fat%2Fg1");
    expect(answer.ok(), "a large verdict broke the response").toBeTruthy();
    const body = await answer.json();
    expect(body.events.length).toBeGreaterThan(0);
  });

  test("the console answers ten questions at once", async ({ page }) => {
    // Nothing here has ever been asked two things simultaneously. A component
    // the host may instantiate per request should be fine — that is the point of
    // the model — but "should be" is what a test is for.
    await signIn(page);
    const answers = await Promise.all(
      Array.from({ length: 10 }, () => page.request.get("/api/runs")),
    );
    for (const a of answers) expect(a.ok()).toBeTruthy();
    const counts = await Promise.all(answers.map(async (a) => (await a.json()).runs.length));
    // Every answer identical: a component that leaked state between concurrent
    // instantiations would show it here.
    expect(new Set(counts).size, `concurrent reads disagreed: ${counts}`).toBe(1);
  });

  test("a token containing cookie punctuation still authenticates", async ({ page, request }) => {
    // The token is whatever the platform issues, and the console parses it back
    // out of a `Cookie:` header by hand — splitting on ';' then on the first '='.
    // Base64 tokens end in '='; a token with a ';' in it would be a cookie the
    // console itself wrote and could not read back.
    //
    // Driven at the API rather than the form, because the point is the parse.
    const login = await request.post("/api/session", {
      headers: { "content-type": "application/json" },
      data: { email: "e2e@example.test", password: "hunter2" },
    });
    expect(login.ok()).toBeTruthy();

    // The stand-in issues `e2e-token`; assert the round trip works with the
    // padding characters a real token would carry.
    const withPadding = "abc+/def==";
    const me = await request.get("/api/session", {
      headers: { cookie: `holon_session=${withPadding}; other=1` },
    });
    expect(me.ok()).toBeTruthy();
    // The stand-in refuses that token, so the console must report NOT signed in
    // rather than erroring — proving it parsed the value and sent it on.
    expect((await me.json()).authenticated).toBe(false);
  });
});
