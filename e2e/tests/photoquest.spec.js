// photoquest, as a photographer uses it: sign up, upload a raw file straight to
// object storage, see it evaluated on the machine with the GPU, share it.
//
// Run the whole stack with `bash e2e/photoquest.sh` (see e2e/README.md). Against
// a stack you started yourself: `npx playwright test tests/photoquest.spec.js`
// with PHOTOQUEST_SAMPLES pointing at the CC0 samples photoquest.sh downloads.
//
// Roles: every account here is a `photographer`. Later roles — `admin`
// (moderation) and `curator` (creates quests, journeys, levels and timed
// competitions) — are not built yet; the scenarios that need them are
// `test.fixme` at the bottom, written down so the next step has its acceptance
// tests before its code.
//
// Media: only CC0 samples from raw.pixls.us (a Sony a7R V, ILCE-7RM5), and a
// JPEG cut out of one of them. Never a personal photo — videos of these runs end
// up in test-results/.

const fs = require('fs');
const { test, expect } = require('@playwright/test');
const pq = require('../lib/photoquest');

const { BASE, SAMPLE } = pq;
const EVALUATE_MS = 120_000;

// One file, so one worker, in order: the evaluator is one queue on one GPU,
// and the resilience scenario depends on knowing what is ahead of it there.
// (Not `mode: 'serial'` — one failure must not skip every scenario after it.)
test.describe.configure({ timeout: 180_000 });

let backend; // comp-media's /health: { apple: bool, ... }

test.beforeAll(async ({ request }) => {
  for (const f of Object.values(SAMPLE)) {
    if (!fs.existsSync(f)) throw new Error(`${f} is missing — run the suite with \`bash e2e/photoquest.sh\`, which downloads the CC0 samples`);
  }
  backend = await pq.mediaHealth(request);
});

// ---- page helpers --------------------------------------------------------------

async function registerInPage(page, who = 'photographer') {
  const email = pq.uniqueEmail(who), password = 'correct horse battery';
  await page.goto(BASE);
  await page.fill('#reg-email', email);
  await page.fill('#reg-password', password);
  await page.click('#register-btn');
  await expect(page.locator('#app')).toBeVisible();
  return { email, password };
}

async function logInInPage(page, { email, password }) {
  await page.fill('#login-email', email);
  await page.fill('#login-password', password);
  await page.click('#login-btn');
}

/** The detail card's value for one row, by its label. */
function field(page, label) {
  return page.locator('#detail dt').filter({ hasText: new RegExp(`^${label}$`) }).locator('xpath=following-sibling::dd[1]');
}

async function uploadInPage(page, file) {
  await page.setInputFiles('#file', file);
  await page.click('#upload-btn');
}

async function expectEvaluatedInPage(page) {
  await expect(page.locator('#upload-status')).toHaveText('evaluated', { timeout: EVALUATE_MS });
  await expect(field(page, 'state')).toHaveText('evaluated');
}

/** Nothing on the detail card reads like a missing value leaking through. */
async function expectNoPlaceholders(page) {
  const text = await page.locator('#detail').innerText();
  expect(text).not.toMatch(/undefined|null|NaN|\[object Object\]/);
}

const token = (page) => page.evaluate(() => localStorage.getItem('token'));

// A 1x1 PNG, made here — a real image of a type the evaluator does not take.
const PNG_1PX = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==',
  'base64',
);

// ---- account ----------------------------------------------------------------------

test.describe('Account', () => {
  test('a new photographer registers and is signed in', async ({ page }) => {
    await registerInPage(page);
    await expect(page.locator('#whoami')).toContainText('signed in as');
    await expect(page.locator('#auth')).toBeHidden();
    await expect(page.locator('#gallery')).toContainText('Nothing yet.');
  });

  test('a photographer logs out and logs in again', async ({ page }) => {
    const who = await registerInPage(page);
    await page.click('#logout-btn');
    await expect(page.locator('#auth')).toBeVisible();
    await expect(page.locator('#app')).toBeHidden();
    // A reload after logging out stays logged out: the token is gone.
    await page.reload();
    await expect(page.locator('#auth')).toBeVisible();
    await logInInPage(page, who);
    await expect(page.locator('#app')).toBeVisible();
    await expect(page.locator('#whoami')).toContainText('signed in as');
  });

  test('a wrong password is refused with a message', async ({ page }) => {
    const who = await registerInPage(page);
    await page.click('#logout-btn');
    await logInInPage(page, { email: who.email, password: 'not the password' });
    await expect(page.locator('#auth-error')).toContainText(/wrong email or password/i);
    await expect(page.locator('#app')).toBeHidden();
  });

  test('registering an email that already has an account is refused', async ({ page }) => {
    const who = await registerInPage(page);
    await page.click('#logout-btn');
    await page.fill('#reg-email', who.email);
    await page.fill('#reg-password', 'another password entirely');
    await page.click('#register-btn');
    await expect(page.locator('#auth-error')).toContainText(/already/i);
    await expect(page.locator('#app')).toBeHidden();
  });
});

// ---- upload & evaluate -------------------------------------------------------------

test.describe('Upload and evaluate', () => {
  test('an uncompressed Sony ARW is evaluated, with its camera data, sharpness, labels and aesthetics', async ({ page }) => {
    await registerInPage(page);
    await uploadInPage(page, SAMPLE.uncompressed);
    await expectEvaluatedInPage(page);

    await expect(page.locator('#detail h3')).toHaveText('7RM5-LosslessUncompressed.ARW');
    await expect(field(page, 'camera')).toHaveText('Sony ILCE-7RM5');
    await expect(field(page, 'lens')).toHaveText('FE 50mm F1.2 GM');
    // e.g. "1/250s · f/1.2 · 50mm · ISO 100"
    await expect(field(page, 'exposure')).toHaveText(/^1\/\d+s · f\/[\d.]+ · [\d.]+mm · ISO \d+$/);
    await expect(field(page, 'size')).toHaveText(/^\d{4} × \d{4}$/);
    await expect(field(page, 'sharpness')).toHaveText(/^focus ratio \d+\.\d \(peak \d+ over a floor of \d+\)$/);
    await expect(page.locator('#detail img')).toHaveJSProperty('complete', true);

    if (backend.apple) {
      // Vision ran: labels and an aesthetics score, and the backend says so.
      await expect(field(page, 'labels')).toHaveText(/\b(grass|outdoor|plant|sky|land|structure)\w* \d+%/);
      await expect(field(page, 'aesthetics')).toHaveText(/^-?\d\.\d\d/);
      await expect(field(page, 'evaluated by')).toHaveText('metal sharpness, coreimage develop, Vision');
    } else {
      // Linux / CI: the CPU path — no Vision stage, so no labels or aesthetics.
      await expect(field(page, 'evaluated by')).toHaveText('cpu sharpness, rawler develop, no Vision');
      await expect(field(page, 'labels')).toHaveCount(0);
    }
    await expectNoPlaceholders(page);
  });

  test('a lossless compressed Sony ARW is evaluated', async ({ page }) => {
    await registerInPage(page);
    await uploadInPage(page, SAMPLE.compressed);
    await expectEvaluatedInPage(page);
    await expect(field(page, 'camera')).toHaveText('Sony ILCE-7RM5');
    await expect(field(page, 'sharpness')).toHaveText(/^focus ratio /);
    await expectNoPlaceholders(page);
  });

  test('a JPEG is evaluated, and its limited metadata is shown without gaps', async ({ page }) => {
    await registerInPage(page);
    await uploadInPage(page, SAMPLE.jpeg);
    await expectEvaluatedInPage(page);
    // EXIF of a JPEG is not read yet (CONTRACT): no camera row rather than an
    // empty or "undefined" one, but its size and sharpness are there.
    await expect(field(page, 'camera')).toHaveCount(0);
    await expect(field(page, 'exposure')).toHaveCount(0);
    await expect(field(page, 'size')).toHaveText(/^\d+ × \d+$/);
    await expect(field(page, 'sharpness')).toHaveText(/^focus ratio /);
    await expect(field(page, 'evaluated by')).toContainText('sharpness');
    await expectNoPlaceholders(page);
  });
});

// ---- the share copy -------------------------------------------------------------------

test.describe('Share copy', () => {
  // How "no serial, no GPS" is checked: lib/photoquest.js `jpegMetadata` walks
  // the JPEG's marker segments up to the first scan and every IFD of an APP1
  // Exif segment. Core Image writes a small structural EXIF block into every
  // JPEG it encodes (dimensions, resolution, colour space), so "no EXIF" is not
  // the bar; instead every tag present must be on an allow-list of structural
  // tags (so Make, Model, lens, dates would fail it too), and none of 0x8825
  // (GPS IFD), 0xA431 (BodySerialNumber), 0x927C (MakerNote: Sony keeps the
  // serial there, encrypted, so a byte search for it would prove nothing) or
  // 0xC634 (Sony's SR2 private data) may appear. No XMP segment either.
  test('the share link opens a JPEG under 10 MB that carries no serial number or GPS', async ({ page, request }) => {
    await registerInPage(page);
    await uploadInPage(page, SAMPLE.uncompressed);
    await expectEvaluatedInPage(page);

    const link = page.locator('#detail a', { hasText: 'Open the share copy' });
    await expect(link).toHaveAttribute('target', '_blank');
    const [tab] = await Promise.all([page.context().waitForEvent('page'), link.click()]);
    await tab.waitForLoadState();
    expect(await tab.evaluate(() => document.contentType)).toBe('image/jpeg');
    await tab.close();

    const res = await request.get(await link.getAttribute('href'));
    expect(res.status()).toBe(200);
    const bytes = await res.body();
    expect(bytes.length).toBeLessThan(10 * 1024 * 1024);
    expect(bytes.length).toBeGreaterThan(100 * 1024);
    const meta = pq.jpegMetadata(bytes);
    expect(meta.identifying).toEqual([]);
    expect(meta.unexpected.map((t) => '0x' + t.toString(16))).toEqual([]);
    expect(meta.app.filter((a) => /xmp|adobe\.com/i.test(a))).toEqual([]);
  });
});

// ---- refusals ---------------------------------------------------------------------------

test.describe('Refusals', () => {
  for (const [what, file] of [
    ['a text file', { name: 'notes.txt', mimeType: 'text/plain', buffer: Buffer.from('not a photo\n') }],
    ['a PNG', { name: 'screenshot.png', mimeType: 'image/png', buffer: PNG_1PX }],
  ]) {
    test(`${what} is refused with a clear message and leaves no photo behind`, async ({ page, request }) => {
      await registerInPage(page);
      await page.setInputFiles('#file', file);
      await page.click('#upload-btn');
      await expect(page.locator('#upload-status')).toContainText(/not accepted.*Sony ARW or JPEG/i);
      await expect(page.locator('#upload-status')).toContainText(file.name);
      await expect(page.locator('#gallery')).toContainText('Nothing yet.');
      expect(await pq.listPhotos(request, await token(page))).toEqual([]);
    });
  }
});

// ---- gallery ----------------------------------------------------------------------------

test.describe('Gallery', () => {
  test('lists newest first, the thumbnails load, and clicking one opens its detail', async ({ page, request }) => {
    const me = await pq.signUp(request);
    const first = await pq.upload(request, me.token, SAMPLE.jpeg, { filename: 'first.jpg' });
    await pq.waitSettled(request, me.token, first);
    const second = await pq.upload(request, me.token, SAMPLE.jpeg, { filename: 'second.jpg' });
    await pq.waitSettled(request, me.token, second);

    await page.goto(BASE);
    await logInInPage(page, me);
    const tiles = page.locator('#gallery .tile');
    await expect(tiles).toHaveCount(2);
    await expect(tiles.locator('.name')).toHaveText(['second.jpg', 'first.jpg']);
    await expect(tiles.locator('.state-evaluated')).toHaveCount(2);
    for (const img of await tiles.locator('img').all()) {
      await expect.poll(() => img.evaluate((i) => i.complete && i.naturalWidth > 0)).toBe(true);
    }

    await tiles.filter({ hasText: 'first.jpg' }).click();
    await expect(page.locator('#detail h3')).toHaveText('first.jpg');
    await expect(field(page, 'state')).toHaveText('evaluated');
  });
});

// ---- resilience -------------------------------------------------------------------------

test.describe('Resilience', () => {
  test('reloading the page while a photo is processing picks it up again and it ends evaluated', async ({ page, request }) => {
    await registerInPage(page);
    const t = await token(page);

    // Queue three raw files ahead of it, so the photo uploaded through the page
    // is certainly still waiting when the page reloads: they are uploaded first
    // and completed together, and the evaluator takes one job at a time.
    const ahead = [];
    for (const f of [SAMPLE.uncompressed, SAMPLE.compressed, SAMPLE.uncompressed]) {
      ahead.push(await pq.upload(request, t, f, { complete: false }));
    }
    for (const a of ahead) await pq.complete(request, t, a);

    await uploadInPage(page, SAMPLE.jpeg);
    await expect(page.locator('#upload-status')).toHaveText(/evaluating/);
    await page.reload();

    await expect(page.locator('#app')).toBeVisible();
    const tile = page.locator('#gallery .tile').filter({ hasText: '7RM5-preview.jpg' });
    await expect(tile.locator('.muted')).toHaveText(/processing|uploaded/);
    // Nothing clicked: the page resumes watching on its own.
    await expect(tile.locator('.muted')).toHaveText('evaluated', { timeout: EVALUATE_MS });
    await expect(tile.locator('img')).toHaveCount(1);
    await expect(page.locator('#detail h3')).toHaveText('7RM5-preview.jpg');
    await expect(field(page, 'state')).toHaveText('evaluated');
    for (const a of ahead) await pq.waitSettled(request, t, a.id);
  });
});

// ---- privacy ----------------------------------------------------------------------------

test.describe('Privacy', () => {
  test("another photographer can neither read nor complete my photo, and does not see it listed", async ({ request }) => {
    const a = await pq.signUp(request, 'alice');
    const b = await pq.signUp(request, 'bob');
    const id = await pq.upload(request, a.token, SAMPLE.jpeg, { filename: 'alices.jpg' });
    await pq.waitSettled(request, a.token, id);

    const read = await pq.getPhoto(request, b.token, id);
    expect(read.status).toBe(403);
    expect(read.body).toEqual({ error: 'forbidden' });
    expect(JSON.stringify(read.body)).not.toContain('renditions');

    const complete = await request.post(`${BASE}/api/photos/${id}/complete`, {
      headers: pq.auth(b.token),
      data: { parts: [{ number: 1, etag: '"x"' }] },
    });
    expect(complete.status()).toBe(403);

    expect(await pq.listPhotos(request, b.token)).toEqual([]);
    expect((await pq.listPhotos(request, a.token)).map((p) => p.id)).toEqual([id]);

    // An id that does not exist is 404, and no login at all is 401.
    expect((await pq.getPhoto(request, b.token, '01K00000000000000000000000')).status).toBe(404);
    expect((await request.get(`${BASE}/api/photos/${id}`)).status()).toBe(401);
  });

  test("on a shared browser, the next photographer sees none of the last one's photos", async ({ page, request }) => {
    const a = await pq.signUp(request, 'alice');
    const b = await pq.signUp(request, 'bob');
    const id = await pq.upload(request, a.token, SAMPLE.jpeg, { filename: 'alices.jpg' });
    await pq.waitSettled(request, a.token, id);

    await page.goto(BASE);
    await logInInPage(page, a);
    await page.locator('#gallery .tile', { hasText: 'alices.jpg' }).click();
    await expect(page.locator('#detail h3')).toHaveText('alices.jpg');
    await page.click('#logout-btn');

    await logInInPage(page, b);
    await expect(page.locator('#app')).toBeVisible();
    await expect(page.locator('#gallery')).toContainText('Nothing yet.');
    await expect(page.locator('#detail')).toBeHidden();
    await expect(page.locator('body')).not.toContainText('alices.jpg');
  });
});

// ---- pending: quests, levels, journeys, competitions, moderation --------------------------
//
// Not built yet (docs/apps/PHOTOQUEST.md, "Where it stands"). Each body is the
// scenario in plain language; the calls get written with the feature. A curator
// is the role that will create quests, journeys and competitions; an admin
// moderates.

test.describe('Quests', () => {
  test.fixme('a photographer sees the active quests: title, what is asked, the XP and the deadline', async () => {
    // Given a curator has created a quest "Something green, in focus", worth 50 XP, ending in a week
    // When the photographer opens the quests view
    // Then the quest is listed with its title, what it asks for, its XP and its deadline
    // And a quest whose deadline has passed is not listed as active
  });

  test.fixme('submitting an evaluated photo to a quest gives a verdict with a reason per requirement', async () => {
    // Given an active quest asking for a "grass" subject with subject sharpness at or above a threshold
    // And the photographer has an evaluated photo
    // When they submit the photo to the quest
    // Then they see a verdict, and one line per requirement:
    //   subject found ✓ (with the label and its confidence)
    //   subject sharpness ≥ threshold ✗ (with the measured value and the threshold)
  });

  test.fixme('a passing photo awards its XP exactly once', async () => {
    // Given a photo that passes an active quest, submitted once — the XP is awarded
    // When the same photo is submitted to the same quest again, the XP does not change
    // And when the same file is uploaded again (same sha256) and submitted, the XP does not change either
    // And the photographer is told why no XP was awarded the second time
  });

  test.fixme('a photo captured before the quest started is refused', async () => {
    // Given a quest that started today
    // And a photo whose captured_at (camera clock) is from before today
    // When the photographer submits it
    // Then it is refused, and the reason names the capture time and the quest's start
  });
});

test.describe('Levels', () => {
  test.fixme('crossing an XP threshold levels the photographer up, and the level shows in the header', async () => {
    // Given a photographer just below the XP needed for level 2
    // When a passing quest submission awards enough XP
    // Then the header shows level 2
    // And the level survives a reload and a fresh login
  });

  test.fixme('the profile shows the XP history', async () => {
    // Given a photographer who has earned XP from two quests
    // When they open their profile
    // Then each award is listed with the quest, the photo, the XP and when, newest first, and the total matches
  });
});

test.describe('Journeys', () => {
  test.fixme('the quests of a journey unlock in order', async () => {
    // Given a curator has created a journey of three quests
    // Then only the first is open; the second and third show as locked
    // When the photographer passes the first, the second unlocks, and the third is still locked
  });

  test.fixme('finishing a journey grants a badge', async () => {
    // Given a photographer has passed all but the last quest of a journey
    // When they pass the last one
    // Then the journey's badge appears on their profile, once
  });
});

test.describe('Timed competitions', () => {
  test.fixme('a photo can be entered before the deadline and is refused after it', async () => {
    // Given a competition a curator created, with a deadline
    // When the photographer enters an evaluated photo before the deadline, the entry is accepted
    // When they try after the deadline, it is refused, and the message names the deadline
  });

  test.fixme('the leaderboard ranks entries by score', async () => {
    // Given three photographers have each entered a photo
    // When anyone opens the competition's leaderboard
    // Then the entries are ordered by score, highest first, each with its photographer and score
  });

  test.fixme('results and winners are visible after the competition ends', async () => {
    // Given a competition whose deadline has passed
    // When a photographer opens it
    // Then it shows as ended, with the final ranking and the winners marked
    // And no further entries are accepted
  });
});

test.describe('Moderation effect', () => {
  test.fixme('a photo an admin hides disappears for others and stops counting, and its owner sees why', async () => {
    // Given a photographer's photo is entered in a quest and on a competition leaderboard
    // When an admin hides it, with a reason
    // Then other photographers no longer see it on the leaderboard or anywhere else
    // And it no longer counts toward the quest or the competition (its XP and rank are withdrawn)
    // And its owner still sees it, marked hidden, with the admin's reason
  });
});
