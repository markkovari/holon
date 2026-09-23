// photoquest, as a photographer uses it: sign up, upload a raw file straight to
// object storage, see it evaluated on the machine with the GPU, share it.
//
// Run the whole stack with `bash e2e/photoquest.sh` (see e2e/README.md). Against
// a stack you started yourself: `npx playwright test tests/photoquest.spec.js`
// with PHOTOQUEST_SAMPLES pointing at the CC0 samples photoquest.sh downloads.
//
// Roles: every account a scenario drives through the page is a `photographer`.
// The game's preconditions — a curator's journeys, quests and competitions, an
// admin's decisions — are set up through the API (lib/photoquest.js); the
// curator's and admin's own pages have their own specs, photoquest-curator and
// photoquest-admin. The admin is the account photoquest.sh names in config
// `bootstrap-admin-email`; deadlines are passed with `POST /test/clock` (config
// `allow-test-routes`), reset after every test.
//
// Which requirements the CC0 samples meet, as the real pipeline reports them:
// a `grass` label at ~0.86–0.90, focus ratio 7.2–9.2, no face, f/1.2, 50 mm,
// ISO 100 (the ARWs; the JPEG has no EXIF), captured 2022-12-17 — so every quest
// here says `captured_after_start: false` except the one that tests that rule.
// XP is paid once per file, ever, so each scenario uploads its own bytes
// (`salt`, lib/photoquest.js) rather than a file another scenario was paid for.
//
// Media: only CC0 samples from raw.pixls.us (a Sony a7R V, ILCE-7RM5), and a
// JPEG cut out of one of them. Never a personal photo — videos of these runs end
// up in test-results/.

const fs = require('fs');
const { test, expect } = require('@playwright/test');
const pq = require('../lib/photoquest');
const ui = require('../lib/photoquest-page');

const { BASE, SAMPLE } = pq;
const EVALUATE_MS = 120_000;

// One worker, in order (photoquest.sh passes --workers=1): the evaluator is one
// queue on one GPU, the resilience scenario depends on knowing what is ahead of
// it there, and the game's clock is one for the whole app.
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

const { registerInPage, logInInPage, tid } = ui;

// The app's clock is one for the whole app: whatever a scenario moved, put back.
test.afterEach(async ({ request }) => {
  await pq.setClock(request, 0);
});

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


// ---- the game: quests, levels, journeys, competitions, moderation ------------------
//
// A curator (granted by the bootstrap admin) sets each scenario up through the
// API; the photographer plays it through the page.

const DAY = 86400;
const tag = () => pq.freshSalt();

/** A photographer with an account, signed in on `page`. */
async function photographerInPage(page, request, who = 'photographer') {
  const me = await pq.signUp(request, who);
  await ui.signIn(page, me);
  return me;
}

/** An evaluated photo of `me`'s, its own bytes (salted) unless `salt` says which. */
function photoOf(request, me, file = SAMPLE.jpeg, opts = {}) {
  return pq.evaluatedPhoto(request, me.token, file, { salt: true, ...opts });
}

async function submitViaApi(request, me, quest, photo) {
  return pq.must(request, me.token, 'POST', `/api/quests/${quest.id}/submissions`, { photo_id: photo.id });
}

test.describe('Quests', () => {
  test('a photographer sees the active quests: title, what is asked, the XP and the deadline', async ({ page, request }) => {
    const cur = await pq.curator(request);
    const t = pq.nowSecs();
    const title = `Into the park ${tag()}`;
    const { quests: [green] } = await pq.publishedJourney(request, cur.token, { title }, [
      { title: 'Something green, in focus', xp: 50, ends_at: t + 7 * DAY, requirements: pq.GRASS },
    ]);
    const pastTitle = `Last week ${tag()}`;
    await pq.publishedJourney(request, cur.token, { title: pastTitle }, [
      { title: 'Yesterday’s light', xp: 30, starts_at: t - 2 * DAY, ends_at: t - DAY, requirements: pq.GRASS },
    ]);
    await photographerInPage(page, request);

    await ui.openJourney(page, title);
    const row = ui.questRow(page, 'Something green, in focus');
    await expect(row).toHaveAttribute('data-state', 'open');
    await expect(row).toContainText('50 XP');
    await expect(row.getByTestId('quest-window')).toHaveText(`open until ${await ui.fmtTime(page, green.ends_at)}`);
    await row.click();
    const reqs = tid(page, 'quest-requirements');
    await expect(reqs).toContainText('Shows “grass” (Vision at least 50% sure)');
    await expect(reqs).toContainText('In focus: focus ratio at least 5');
    await expect(reqs).not.toContainText('Taken after');
    await expect(tid(page, 'submit-photo-btn').or(page.getByText('no evaluated photos yet'))).toBeVisible();

    // A quest whose deadline has passed is shown as ended, not as something to do.
    await ui.openJourney(page, pastTitle);
    const past = ui.questRow(page, 'Yesterday’s light');
    await expect(past.getByTestId('quest-window')).toHaveText(/^ended /);
    await past.click();
    await expect(tid(page, 'quest-detail')).toContainText('This quest ended');
    await expect(tid(page, 'submit-photo-btn')).toHaveCount(0);
  });

  test('submitting an evaluated photo to a quest gives a verdict with a reason per requirement', async ({ page, request }) => {
    const cur = await pq.curator(request);
    const title = `Sharp greens ${tag()}`;
    await pq.publishedJourney(request, cur.token, { title }, [{
      title: 'Very sharp grass', xp: 50,
      requirements: { ...pq.passable(backend), sharpness: { min_focus_ratio: 50 }, exposure: { max_iso: 3200 } },
    }]);
    const me = await photographerInPage(page, request);
    const photo = await photoOf(request, me, SAMPLE.jpeg);

    await ui.openQuest(page, title, 'Very sharp grass');
    const verdict = await ui.submitPhoto(page, photo.id);
    await expect(tid(page, 'verdict-result')).toHaveText('Not passed');
    if (backend.apple) {
      // subject found ✓, with the label and its confidence against the minimum
      await expect(ui.check(verdict, 'subject')).toHaveAttribute('data-ok', 'true');
      await expect(ui.check(verdict, 'subject')).toContainText(/✓Subject: grass 0\.\d\d ≥ 0\.50/);
    }
    // sharpness ✗, with the measured value and the threshold
    await expect(ui.check(verdict, 'sharpness.min_focus_ratio')).toHaveAttribute('data-ok', 'false');
    await expect(ui.check(verdict, 'sharpness.min_focus_ratio')).toContainText(/✗Focus: \d+\.\d+ < 50\.0/);
    // and a check whose input this JPEG does not carry is "not looked at", not failed
    await expect(ui.check(verdict, 'exposure.max_iso')).toHaveAttribute('data-ok', 'null');
    await expect(ui.check(verdict, 'exposure.max_iso')).toContainText(/—ISO: the photo's metadata has no iso/);
    await expect(tid(page, 'xp-line')).toHaveText('No XP — the photo did not pass every check.');
    await expect(tid(page, 'header-total-xp')).toHaveText('0 XP');
  });

  test('a passing photo awards its XP exactly once', async ({ page, request }) => {
    const cur = await pq.curator(request);
    const title = `Green twice ${tag()}`;
    await pq.publishedJourney(request, cur.token, { title }, [
      { title: 'Green, once', xp: 40, requirements: pq.passable(backend) },
      { title: 'Green, again', xp: 25, requirements: pq.passable(backend) },
    ]);
    const me = await photographerInPage(page, request);
    const salt = tag();
    const photo = await photoOf(request, me, SAMPLE.jpeg, { salt, filename: 'green.jpg' });

    await ui.openQuest(page, title, 'Green, once');
    await ui.submitPhoto(page, photo.id);
    await expect(tid(page, 'verdict-result')).toHaveText('Passed');
    await expect(tid(page, 'xp-line')).toHaveText('+40 XP');
    await expect(tid(page, 'header-total-xp')).toHaveText('40 XP');

    // The same photo, the same quest: judged again, not paid again.
    await ui.submitPhoto(page, photo.id);
    await expect(tid(page, 'verdict-result')).toHaveText('Passed');
    await expect(tid(page, 'xp-line')).toContainText('no XP this time: this file has already earned XP once');
    await expect(tid(page, 'header-total-xp')).toHaveText('40 XP');

    // The same bytes uploaded again (same sha256), to the next quest: it passes,
    // and unlocks what comes after, but earns nothing — a file is paid once, ever.
    const copy = await photoOf(request, me, SAMPLE.jpeg, { salt, filename: 'green-copy.jpg' });
    expect(copy.sha256).toBe(photo.sha256);
    await ui.openQuest(page, title, 'Green, again');
    await ui.submitPhoto(page, copy.id);
    await expect(tid(page, 'verdict-result')).toHaveText('Passed');
    await expect(tid(page, 'xp-line')).toContainText('this file has already earned XP once (the same photo, or the same bytes uploaded again)');
    await expect(tid(page, 'header-total-xp')).toHaveText('40 XP');
    await expect(ui.questRow(page, 'Green, again')).toHaveAttribute('data-state', 'passed');
    expect((await pq.must(request, me.token, 'GET', '/api/me/progress')).total_xp).toBe(40);
  });

  test('a photo captured before the quest started is refused', async ({ page, request }) => {
    const cur = await pq.curator(request);
    const title = `From today ${tag()}`;
    const { captured_after_start, ...rest } = pq.passable(backend); // eslint-disable-line no-unused-vars
    const { quests: [quest] } = await pq.publishedJourney(request, cur.token, { title }, [
      { title: 'Taken today', xp: 50, requirements: rest }, // captured_after_start: the default, true
    ]);
    const me = await photographerInPage(page, request);
    const photo = await photoOf(request, me, SAMPLE.uncompressed); // camera clock: 2022-12-17

    await ui.openQuest(page, title, 'Taken today');
    await expect(tid(page, 'quest-requirements')).toContainText(`Taken after the quest started (${await ui.fmtTime(page, quest.starts_at)})`);
    const verdict = await ui.submitPhoto(page, photo.id);
    await expect(tid(page, 'verdict-result')).toHaveText('Not passed');
    const when = ui.check(verdict, 'captured_after_start');
    await expect(when).toHaveAttribute('data-ok', 'false');
    // the reason names the capture time and the quest's start
    await expect(when).toContainText(/captured 2022-12-17T16:03:31 < start \d{4}-\d\d-\d\dT\d\d:\d\d:\d\d/);
    await expect(tid(page, 'xp-line')).toContainText('No XP');
  });
});

test.describe('Levels', () => {
  test('crossing an XP threshold levels the photographer up, and the level shows in the header', async ({ page, request }) => {
    const cur = await pq.curator(request);
    const title = `Two steps ${tag()}`;
    const { journey, quests: [first] } = await pq.publishedJourney(request, cur.token,
      { title, levels: [{ level: 1, xp: 0 }, { level: 2, xp: 100 }] }, [
        { title: 'Step one', xp: 60, requirements: pq.passable(backend) },
        { title: 'Step two', xp: 60, requirements: pq.passable(backend) },
      ]);
    const me = await pq.signUp(request);
    await submitViaApi(request, me, first, await photoOf(request, me)); // 60 XP: just below 100
    const photo = await photoOf(request, me);

    await ui.signIn(page, me);
    const level = page.locator(`[data-testid=header-level][data-journey="${journey.id}"]`);
    await expect(level).toHaveText(`${title}: level 1`);
    await ui.openQuest(page, title, 'Step two');
    await ui.submitPhoto(page, photo.id);
    await expect(tid(page, 'level-up')).toHaveText(`Level up! ${title}: level 1 → level 2`);
    await expect(level).toHaveText(`${title}: level 2`);
    await expect(tid(page, 'header-total-xp')).toHaveText('120 XP');

    await page.reload();
    await expect(level).toHaveText(`${title}: level 2`);
    await page.click('#logout-btn');
    await logInInPage(page, me);
    await expect(level).toHaveText(`${title}: level 2`);
  });

  test('the profile shows the XP history', async ({ page, request }) => {
    const cur = await pq.curator(request);
    const title = `History ${tag()}`;
    const { quests: [a, b] } = await pq.publishedJourney(request, cur.token, { title }, [
      { title: `Morning grass ${tag()}`, xp: 30, requirements: pq.passable(backend) },
      { title: `Evening grass ${tag()}`, xp: 20, requirements: pq.passable(backend) },
    ]);
    const me = await pq.signUp(request);
    await submitViaApi(request, me, a, await photoOf(request, me, SAMPLE.jpeg, { filename: 'morning.jpg' }));
    await submitViaApi(request, me, b, await photoOf(request, me, SAMPLE.jpeg, { filename: 'evening.jpg' }));

    await ui.signIn(page, me);
    await ui.nav(page, 'progress');
    await expect(tid(page, 'total-xp')).toHaveText('50 XP');
    const rows = tid(page, 'ledger-row');
    await expect(rows).toHaveCount(2);
    // newest first: the quest, the photo, the XP, and when
    await expect(rows.nth(0)).toContainText(`Quest “${b.title}”`);
    await expect(rows.nth(0)).toContainText('evening.jpg');
    await expect(rows.nth(0).getByTestId('ledger-xp')).toHaveText('+20');
    await expect(rows.nth(0)).toContainText(/\d{4}-\d\d-\d\d \d\d:\d\d/);
    await expect(rows.nth(1)).toContainText(`Quest “${a.title}”`);
    await expect(rows.nth(1)).toContainText('morning.jpg');
    await expect(rows.nth(1).getByTestId('ledger-xp')).toHaveText('+30');
    await expect(tid(page, 'progress-journey').filter({ hasText: title })).toContainText('50');
  });
});

test.describe('Journeys', () => {
  test('the quests of a journey unlock in order', async ({ page, request }) => {
    const cur = await pq.curator(request);
    const title = `Three in a row ${tag()}`;
    await pq.publishedJourney(request, cur.token, { title }, ['First', 'Second', 'Third'].map((t) => (
      { title: t, xp: 10, requirements: pq.passable(backend) })));
    const me = await photographerInPage(page, request);
    const photo = await photoOf(request, me);

    await ui.openJourney(page, title);
    const states = () => tid(page, 'quest-row').evaluateAll((rows) => rows.map((r) => r.dataset.state));
    await expect.poll(states).toEqual(['open', 'locked', 'locked']);
    await expect(ui.questRow(page, 'Second')).toContainText('Locked — pass “First” first');
    // A locked quest does not open.
    await ui.questRow(page, 'Third').click();
    await expect(tid(page, 'quest-detail')).toHaveCount(0);

    await ui.questRow(page, 'First').click();
    await ui.submitPhoto(page, photo.id);
    await expect(tid(page, 'verdict-result')).toHaveText('Passed');
    await expect.poll(states).toEqual(['passed', 'open', 'locked']);
  });

  test('finishing a journey grants a badge', async ({ page, request }) => {
    const cur = await pq.curator(request);
    const title = `Ranger ${tag()}`;
    const badge = `Park ranger ${tag()}`;
    const { quests: [first, last] } = await pq.publishedJourney(request, cur.token, { title, badge: { name: badge } }, [
      { title: 'Almost', xp: 10, requirements: pq.passable(backend) },
      { title: 'The last one', xp: 10, requirements: pq.passable(backend) },
    ]);
    const me = await pq.signUp(request);
    await submitViaApi(request, me, first, await photoOf(request, me));
    const photo = await photoOf(request, me);

    await ui.signIn(page, me);
    await ui.openQuest(page, title, 'The last one');
    await ui.submitPhoto(page, photo.id);
    await expect(tid(page, 'badge-notice')).toHaveText(`Badge earned: ${badge}`);
    await expect(tid(page, 'journey-detail')).toContainText(`Badge earned: ${badge}`);

    // Passing it again grants nothing more.
    const again = await submitViaApi(request, me, last, await photoOf(request, me));
    expect(again.pass).toBe(true);
    expect(again.badge).toBeNull();
    await ui.nav(page, 'progress');
    await expect(tid(page, 'badge')).toHaveCount(1);
    await expect(tid(page, 'badge')).toContainText(`${badge} — ${title}`);
  });
});

test.describe('Timed competitions', () => {
  test('a photo can be entered before the deadline and is refused after it', async ({ page, request }) => {
    const cur = await pq.curator(request);
    const title = `Before noon ${tag()}`;
    const comp = await pq.publishedCompetition(request, cur.token, {
      title, requirements: pq.passable(backend), max_entries_per_user: 2,
    });
    const me = await photographerInPage(page, request);
    const [a, b] = [await photoOf(request, me), await photoOf(request, me)];

    await ui.openCompetition(page, title);
    await expect(tid(page, 'competition-phase')).toHaveText('open for entries');
    await tid(page, 'enter-photo-select').selectOption(a.id);
    await tid(page, 'enter-btn').click();
    await expect(tid(page, 'enter-ok')).toHaveText('Entered — good luck.');
    await expect(tid(page, 'lb-row')).toHaveCount(1);

    // The deadline passes while the page is still open.
    await pq.setClock(request, comp.closes_at - pq.nowSecs() + 60);
    await tid(page, 'enter-photo-select').selectOption(b.id);
    await tid(page, 'enter-btn').click();
    const deadline = await ui.fmtTime(page, comp.closes_at);
    await expect(tid(page, 'enter-error')).toHaveText(`Not entered: Entries closed at ${deadline}.`);

    await ui.openCompetition(page, title);
    await expect(tid(page, 'competition-phase')).toHaveText('voting');
    await expect(tid(page, 'enter-closed')).toHaveText(`Entries closed at ${deadline}.`);
    await expect(tid(page, 'enter-btn')).toHaveCount(0);
  });

  test('the leaderboard ranks entries by score', async ({ page, request }) => {
    const cur = await pq.curator(request);
    const title = `Crowd favourite ${tag()}`;
    const comp = await pq.publishedCompetition(request, cur.token, {
      title, requirements: pq.passable(backend), weights: { auto: 0.2, votes: 0.8, judges: 0 },
    });
    const entrants = [];
    for (const [who, file] of [['ana', SAMPLE.uncompressed], ['ben', SAMPLE.compressed], ['cleo', SAMPLE.jpeg]]) {
      const u = await pq.signUp(request, who);
      const photo = await photoOf(request, u, file);
      await pq.must(request, u.token, 'POST', `/api/competitions/${comp.id}/entries`, { photo_id: photo.id });
      entrants.push({ ...u, name: u.email.split('@')[0] });
    }
    const [ana, ben, cleo] = entrants;

    // A fourth photographer votes through the page: 5 stars for cleo, 3 for ben, 1 for ana.
    await photographerInPage(page, request, 'voter');
    await ui.openCompetition(page, title);
    for (const [who, stars] of [[cleo, 5], [ben, 3], [ana, 1]]) {
      await tid(page, 'lb-row').filter({ hasText: who.name }).getByTestId(`star-${stars}`).click();
      await expect(tid(page, 'vote-ok')).toHaveText(`Vote saved: ${stars} of 5.`);
    }

    const rows = tid(page, 'lb-row');
    await expect(rows).toHaveCount(3);
    await expect(rows.getByTestId('lb-entrant')).toHaveText([cleo.name, ben.name, ana.name]);
    await expect(rows.getByTestId('lb-rank')).toHaveText(['1', '2', '3']);
    const scores = (await rows.getByTestId('lb-score').allInnerTexts()).map(Number);
    expect(scores).toEqual([...scores].sort((x, y) => y - x));
    expect(scores[0]).toBeGreaterThan(scores[1]);
    await expect(rows.first()).toContainText('votes 1.00 ×80% (1 vote, mean 5.0★)');
    await expect(rows.first()).toContainText('judges ×0%: none yet');
    // Everyone sees the same ranking, the entrant's own row marked.
    await ui.signIn(page, cleo);
    await ui.openCompetition(page, title);
    await expect(tid(page, 'lb-entrant')).toHaveText([`${cleo.name} (you)`, ben.name, ana.name]);
    await expect(tid(page, 'lb-row').first()).toContainText('your entry');
    await expect(tid(page, 'lb-row').first().getByTestId('star-1')).toHaveCount(0);
  });

  test('results and winners are visible after the competition ends', async ({ page, request }) => {
    const cur = await pq.curator(request);
    const title = `Short and sweet ${tag()}`;
    const t = pq.nowSecs();
    const comp = await pq.publishedCompetition(request, cur.token, {
      title, requirements: pq.passable(backend), prizes_xp: [300, 200],
      closes_at: t + 600, voting_closes_at: t + 1200, judging_closes_at: t + 1800,
    });
    const ana = await pq.signUp(request, 'ana');
    const ben = await pq.signUp(request, 'ben');
    // auto-v1 only: the ARW (focus 9.2, sharper) above the JPEG preview (7.2)
    const anaPhoto = await photoOf(request, ana, SAMPLE.uncompressed);
    await pq.must(request, ana.token, 'POST', `/api/competitions/${comp.id}/entries`, { photo_id: anaPhoto.id });
    const benPhoto = await photoOf(request, ben, SAMPLE.jpeg);
    await pq.must(request, ben.token, 'POST', `/api/competitions/${comp.id}/entries`, { photo_id: benPhoto.id });

    await pq.setClock(request, comp.judging_closes_at - pq.nowSecs() + 60);
    await ui.signIn(page, ana);
    await ui.openCompetition(page, title);
    await expect(tid(page, 'competition-phase')).toHaveText('ended');
    const [anaName, benName] = [ana.email.split('@')[0], ben.email.split('@')[0]];
    await expect(tid(page, 'winner')).toHaveText([
      `1st place: ${anaName} (you) — 300 XP`,
      `2nd place: ${benName} — 200 XP`,
    ]);
    await expect(tid(page, 'result-row')).toHaveCount(2);
    await expect(tid(page, 'result-row').first()).toContainText(`${anaName} (you) — winner`);
    await expect(tid(page, 'enter-closed')).toContainText('Entries closed at');

    // No further entries, and the prize is on the winner's ledger (once).
    const late = await pq.call(request, ben.token, 'POST', `/api/competitions/${comp.id}/entries`, {
      photo_id: (await photoOf(request, ben)).id,
    });
    expect(late.status).toBe(409);
    expect(late.body.error).toBe('competition_closed');
    await ui.nav(page, 'progress');
    await expect(tid(page, 'total-xp')).toHaveText('300 XP');
    await expect(tid(page, 'ledger-row')).toHaveCount(1);
    await expect(tid(page, 'ledger-row')).toContainText(`Competition “${title}”, 1st place`);
  });
});

test.describe('Moderation effect', () => {
  // CONTRACT.md "Moderation": a hidden photo leaves every shared surface and the
  // ranking and cannot be submitted or entered again; "its past XP stays (the
  // ledger is history)". That last part is the contract's decision, and what is
  // asserted here.
  test('a photo an admin hides disappears for others and stops counting, and its owner sees why', async ({ page, browser, request }) => {
    const cur = await pq.curator(request);
    const adm = await pq.admin(request);
    const jTitle = `Moderated ${tag()}`;
    const { quests: [quest, next] } = await pq.publishedJourney(request, cur.token, { title: jTitle }, [
      { title: 'Paid before', xp: 30, requirements: pq.passable(backend) },
      { title: 'Not any more', xp: 30, requirements: pq.passable(backend) },
    ]);
    const cTitle = `Leaderboard ${tag()}`;
    const comp = await pq.publishedCompetition(request, cur.token, { title: cTitle, requirements: pq.passable(backend) });
    const owner = await pq.signUp(request, 'owner');
    const photo = await photoOf(request, owner, SAMPLE.jpeg, { filename: 'mine.jpg' });
    await submitViaApi(request, owner, quest, photo);
    await pq.must(request, owner.token, 'POST', `/api/competitions/${comp.id}/entries`, { photo_id: photo.id });
    const other = await pq.signUp(request, 'other');
    const otherPhoto = await photoOf(request, other, SAMPLE.uncompressed);
    await pq.must(request, other.token, 'POST', `/api/competitions/${comp.id}/entries`, { photo_id: otherPhoto.id });
    const ownerName = owner.email.split('@')[0];

    const viewer = await ui.secondBrowser(browser, other);
    await ui.openCompetition(viewer, cTitle);
    await expect(tid(viewer, 'lb-row')).toHaveCount(2);
    await expect(tid(viewer, 'lb-row').filter({ hasText: ownerName })).toHaveCount(1);

    const reason = 'Not the photographer’s own work';
    await pq.must(request, adm.token, 'POST', `/api/admin/photos/${photo.id}/hide`, { reason });

    // Others: gone from the leaderboard, and from everywhere else (a report is a 404).
    await ui.openCompetition(viewer, cTitle);
    await expect(tid(viewer, 'lb-row')).toHaveCount(1);
    await expect(tid(viewer, 'lb-row').filter({ hasText: ownerName })).toHaveCount(0);
    await expect(tid(viewer, 'lb-rank')).toHaveText(['1']);
    expect((await pq.call(request, other.token, 'POST', `/api/photos/${photo.id}/reports`, { reason: 'spam' })).status).toBe(404);
    expect((await pq.getPhoto(request, other.token, photo.id)).status).toBe(403);

    // It stops counting: not submittable, not in the owner's pickers.
    const refused = await pq.call(request, owner.token, 'POST', `/api/quests/${next.id}/submissions`, { photo_id: photo.id });
    expect(refused.status).toBe(409);
    expect(refused.body.error).toBe('photo_hidden');

    // The owner still sees it, marked hidden, with the admin's reason.
    await ui.signIn(page, owner);
    const tile = page.locator('#gallery .tile', { hasText: 'mine.jpg' });
    await expect(tile).toContainText('hidden');
    await tile.click();
    await expect(tid(page, 'moderation-notice')).toContainText(`Hidden by a moderator: ${reason}`);
    await ui.openQuest(page, jTitle, 'Not any more');
    await expect(page.getByText('You have no evaluated photos yet')).toBeVisible();
    // Its past XP stays: the ledger is history.
    await expect(tid(page, 'header-total-xp')).toHaveText('30 XP');
    await viewer.context().close();
  });
});
