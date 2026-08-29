// Screencast: a 24-hour reminder, on the REAL running app, with a REAL mailbox.
//
// Three panes, all of them live. Left and centre are the same SPA under
// `?as=attendee` and `?as=organizer`, served from the app's own static dir. Right is
// MailHog's own UI — not a screenshot of one, and not a mock: `comp-mailrelay` turns
// the component's HTTP POST into a genuine SMTP session, because `comp-host` wires
// no `wasi:sockets` and a component cannot speak SMTP at all.
//
// What the recording is meant to say, in order:
//
//   * the organizer opens an event, and its reminder goes on the clock at
//     `starts_at` minus 24 hours — `sched:timer` holds it;
//   * the attendee holds a ticket and has opted into BOTH channels;
//   * the clock ticks. In a deployment that is `comp-relay` on a schedule hitting
//     the same route; here it is a button, which is the same work;
//   * the badge moves on the LEFT without anybody reloading it — an SSE frame,
//     through a stream opened with a signed 60-second ticket because `EventSource`
//     cannot send an Authorization header;
//   * and the email is in the mailbox on the RIGHT.
//
// The event starts in two hours, so its reminder is already due. The schedule is
// real; only the clock is chosen — waiting a day would prove the same thing slower.
//
// Prereq: MailHog on :8025, `just mail-relay &`, and the app on :3230.
import { chromium } from "playwright";

const BASE = process.env.EVENTS_URL || "http://127.0.0.1:3230";
const OUT = new URL("./videos/events-reminder/", import.meta.url).pathname;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const soon = new Date(Date.now() + 2 * 3600 * 1000).toISOString().slice(0, 16);
// The title carries the run, and the attendee claims THAT card. `.first()` picks
// whichever event renders first, which on a host that has recorded before is a
// stale one whose reminder already fired — `fired: 0`, and a badge that never moves.
// Unique per run. Two events with one title is a strict-mode violation in
// Playwright and, worse, an ambiguous claim: the attendee would take a ticket for
// whichever card rendered first, and its reminder has already fired.
const TITLE = `Wasm & Free Drinks #${String(Date.now()).slice(-4)}`;

const browser = await chromium.launch();
const ctx = await browser.newContext({
  viewport: { width: 1440, height: 660 },
  recordVideo: { dir: OUT, size: { width: 1440, height: 660 } },
  deviceScaleFactor: 2,
});
const page = await ctx.newPage();
await page.goto(`${BASE}/split.html`);

const att = page.frameLocator("#attendee");
const org = page.frameLocator("#organizer");

// --- both sign in ------------------------------------------------------------
// Create, then fall back to signing in. The host's kv is in memory, so a restart
// empties it — and a screencast that assumes an account exists fails on a clean
// one, which looks exactly like the app being broken.
// The attendee is fresh each run. The organizer cannot be — `organizer-emails` in
// the app spec names one address, and a role is granted by the deployment rather
// than claimed by whoever signs up. A reused attendee would already hold a ticket
// and the claim would be a 409, which reads as the app being broken.
const attendee = `ada-${Date.now()}@example.test`;
for (const [frame, email] of [[att, attendee], [org, "boss@example.test"]]) {
  await frame.locator("text=Free tickets").waitFor({ timeout: 25000 });
  await frame.locator("input[type=email]").fill(email);
  await frame.locator("input[type=password]").fill("correct-horse");
  await frame.locator("button:has-text('Create account')").click();
  await sleep(1200);
  if (await frame.locator("text=that email is taken").count()) {
    await frame.locator("button:has-text('Sign in')").click();
  }
}
await att.locator("text=Open events").waitFor({ timeout: 25000 });
await org.locator("text=Open an event").waitFor({ timeout: 25000 });
await sleep(1200);

// --- the organizer opens an event that starts tonight -------------------------
await org.locator("button:has-text('+ Open an event')").click();
await org.locator("input[placeholder='what is it called']").fill(TITLE);
await org.locator("textarea").fill("Posters, QR codes and a hard capacity. Bring nothing.");
await org.locator("input[type=datetime-local]").fill(soon);
await sleep(600);
await org.locator("button:has-text('Open it')").click();
await org.locator(`text=${TITLE}`).waitFor({ timeout: 25000 });
await sleep(1400);

// --- the attendee takes a place ------------------------------------------------
await page.evaluate(() => document.getElementById("attendee").contentWindow.location.reload());
await att.locator("text=Open events").waitFor({ timeout: 25000 });
// The card for the event just opened, not whichever renders first.
await att
  .locator("div", { hasText: TITLE })
  .locator("button:has-text('Claim')")
  .last()
  .click();
await att.locator("text=ticket issued").waitFor({ timeout: 25000 });
await sleep(1200);

// --- and asks for email as well as in-app ---------------------------------------
//
// The default is in-app only, which is the setting that cannot deliver anything
// anywhere it should not — so wanting email is something a person does, on purpose,
// with the address they chose. This is the click that puts a message in the mailbox
// on the right.
await att.locator("button[aria-label='notifications']").click();
await att.locator("text=email me too").waitFor({ timeout: 15000 });
await att.locator("input[type=checkbox]").check();
await sleep(1400);
await att.locator("button[aria-label='notifications']").click();
await sleep(800);

// --- the clock ticks -----------------------------------------------------------
await org.locator("button:has-text('run due reminders')").click();
await org.locator("text=sent 1 reminder").waitFor({ timeout: 25000 });
await sleep(1800);

// --- the badge moved on its own, and the mail is real ---------------------------
await att.locator("button[aria-label='notifications'] span").waitFor({ timeout: 20000 });
await sleep(900);
await att.locator("button[aria-label='notifications']").click();
await att.locator(`text=Tomorrow: ${TITLE}`).waitFor({ timeout: 20000 });
await sleep(2000);

// Through Playwright's frame API, not `contentWindow.location.reload()`: the
// mailbox is a different origin (:8025 against the app's :3230) and the browser
// blocks the page from touching it. Which is the correct behaviour and the reason
// this pane is real rather than something the app drew.
const mail = page.frames().find((f) => f.url().includes("8025"));
if (mail) await mail.goto(mail.url());
await sleep(3400);

await ctx.close();
await browser.close();
console.log(`recorded to ${OUT}`);
