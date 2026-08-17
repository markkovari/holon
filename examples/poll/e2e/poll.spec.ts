import { expect, test, type Browser, type Page } from "@playwright/test";

// What is asserted here and NOT in an API test:
//
//   * one vote per browser is a cookie rule, so it needs two cookie jars. One HTTP
//     client either sends the cookie it was just given or never sends one — both
//     answers are wrong and both look like a pass.
//   * the chart is an SVG a component rendered and the page embedded. `<svg>` in a
//     response body proves the renderer; `<svg>` in the DOM proves the page put it
//     there, which is a different claim and the one that breaks.
//   * a poll survives a reload, because the votes are in a store and not in a tab.
//
// Everything below drives the real pages. There is no fixture and no mock.

/// Create a poll through the UI and return its code.
async function createPoll(page: Page, question: string, options: string[]): Promise<string> {
  await page.goto("/");
  await page.fill("#q", question);
  await page.fill("#opts", options.join(", "));
  await page.click("#make");
  const made = page.locator("#made");
  await expect(made).toBeVisible();
  const code = await made.getAttribute("data-code");
  expect(code, "the page must report the poll's code").toBeTruthy();
  return code as string;
}

/// A page in its OWN browser context — a separate cookie jar, which is what makes
/// it a different voter.
async function freshVoter(browser: Browser, code: string): Promise<Page> {
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  await page.goto(`/p/${code}`);
  return page;
}

test("a poll is created, shared, and shows a QR for its link", async ({ page }) => {
  const code = await createPoll(page, "Which one?", ["Rust", "Go", "Zig"]);

  // The share link is the URL a person reads out loud, so it has to contain the code.
  await expect(page.locator("#link")).toContainText(`/p/${code}`);

  // The QR is a document `qr:encode` rendered. Asserting it LOADED, not merely that
  // the element exists: a broken src is an <img> that still passes a DOM check.
  const qr = page.locator("#qr");
  await expect(qr).toBeVisible();
  const loaded = await qr.evaluate((img: HTMLImageElement) => img.complete && img.naturalWidth > 0);
  expect(loaded, "the QR image must actually load").toBe(true);
});

test("a question needs at least two options", async ({ page }) => {
  await page.goto("/");
  await page.fill("#q", "Yes?");
  await page.fill("#opts", "Only one");
  await page.click("#make");
  // The refusal is the app's, not the browser's: no `required` attributes, the
  // component decides and the page reports what it said.
  await expect(page.locator("#err")).toContainText("at least two options");
  await expect(page.locator("#made")).toBeHidden();
});

test("two browsers vote once each; a third vote from the same browser is refused", async ({
  page,
  browser,
}) => {
  const code = await createPoll(page, "Pick a language", ["Rust", "Go"]);

  // --- first browser -----------------------------------------------------------
  const a = await freshVoter(browser, code);
  await expect(a.locator("#q")).toHaveText("Pick a language");
  await a.click('button.opt[data-option="Rust"]');
  await expect(a.locator("#note")).toHaveAttribute("data-state", "voted");
  await expect(a.locator("#note")).toContainText("1 vote so far");

  // Voting again in the SAME browser is refused, and the page says so rather than
  // silently doing nothing.
  await a.click('button.opt[data-option="Go"]');
  await expect(a.locator("#err")).toContainText("already voted");
  await expect(a.locator("#note")).toHaveAttribute("data-state", "already");
  // And the count did not move.
  await expect(a.locator("#chart")).toHaveAttribute("data-total", "1");

  // --- second browser, second cookie jar ---------------------------------------
  const b = await freshVoter(browser, code);
  // It must NOT think it has already voted — this is the assertion that fails if the
  // voter id is anything shared, like a fixed string or the poll's own id.
  await expect(b.locator("#err")).toHaveText("");
  await b.click('button.opt[data-option="Go"]');
  await expect(b.locator("#note")).toContainText("2 votes so far");
  await expect(b.locator("#chart")).toHaveAttribute("data-total", "2");

  // --- the first browser sees the second's vote after a reload ------------------
  await a.reload();
  await expect(a.locator("#chart")).toHaveAttribute("data-total", "2");
  // Still refused after the reload: the cookie outlives the page.
  await a.click('button.opt[data-option="Rust"]');
  await expect(a.locator("#err")).toContainText("already voted");

  await a.context().close();
  await b.context().close();
});

test("the results are a server-rendered SVG in the page, with every option labelled", async ({
  page,
  browser,
}) => {
  const code = await createPoll(page, "Best editor", ["vim", "emacs", "helix"]);
  const voter = await freshVoter(browser, code);
  await voter.click('button.opt[data-option="helix"]');
  await expect(voter.locator("#note")).toHaveAttribute("data-state", "voted");

  // An <svg> element IN THE DOM, not a string in a response. The page fetched the
  // document and embedded it; there is no charting library on the page to do this.
  const svg = voter.locator("#chart svg");
  await expect(svg).toBeVisible();

  // Every option is labelled in the chart the component drew.
  for (const label of ["vim", "emacs", "helix"]) {
    await expect(voter.locator("#chart")).toContainText(label);
  }
  await voter.context().close();
});

test("an unknown code says so instead of looking broken", async ({ page }) => {
  await page.goto("/p/NOSUCH");
  await expect(page.locator("#q")).toHaveText("No such poll.");
  // No options to click, and no error either: this is a stated outcome, not a fault.
  await expect(page.locator("button.opt")).toHaveCount(0);
});
