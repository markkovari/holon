// photoquest, as an admin uses it: work the reports queue (hide, dismiss,
// unhide), grant and revoke roles, suspend and unsuspend accounts — each seen
// from the other side too: the photographer who reported, the owner of a hidden
// photo, the account that was granted or suspended.
//
// Run with `bash e2e/photoquest.sh` (all three photoquest specs). The admin is
// the account photoquest.sh names in config `bootstrap-admin-email` — admin from
// the moment it registers (lib/photoquest.js `admin`). Curators and
// competitions a scenario only needs as a precondition are made through the API.
//
// Media: only the CC0 samples photoquest.sh downloads (raw.pixls.us).

const fs = require('fs');
const { test, expect } = require('@playwright/test');
const pq = require('../lib/photoquest');
const ui = require('../lib/photoquest-page');

const { SAMPLE } = pq;
const { tid } = ui;

test.describe.configure({ timeout: 180_000 });

let backend;
test.beforeAll(async ({ request }) => {
  for (const f of Object.values(SAMPLE)) {
    if (!fs.existsSync(f)) throw new Error(`${f} is missing — run the suite with \`bash e2e/photoquest.sh\``);
  }
  backend = await pq.mediaHealth(request);
});

test.afterEach(async ({ request }) => {
  await pq.setClock(request, 0);
});

const tag = () => pq.freshSalt();
const nameOf = (u) => u.email.split('@')[0];

/** The bootstrap admin, signed in on `page`, on the Admin tab. */
async function adminInPage(page, request) {
  const adm = await pq.admin(request);
  await ui.signIn(page, adm);
  await expect(tid(page, 'nav-admin')).toBeVisible();
  await ui.nav(page, 'admin');
  return adm;
}

/** A published competition with `owner`'s photo and another entrant's in it. */
async function competitionWith(request, owner, filename) {
  const cur = await pq.curator(request);
  const title = `Reported ${tag()}`;
  const comp = await pq.publishedCompetition(request, cur.token, { title, requirements: pq.passable(backend) });
  const photo = await pq.evaluatedPhoto(request, owner.token, SAMPLE.jpeg, { salt: true, filename });
  await pq.must(request, owner.token, 'POST', `/api/competitions/${comp.id}/entries`, { photo_id: photo.id });
  const other = await pq.signUp(request, 'other');
  const p2 = await pq.evaluatedPhoto(request, other.token, SAMPLE.compressed, { salt: true });
  await pq.must(request, other.token, 'POST', `/api/competitions/${comp.id}/entries`, { photo_id: p2.id });
  return { comp, title, photo };
}

function userRow(page, user) {
  return page.locator(`[data-testid=user-row][data-email="${user.email}"]`);
}

test.describe('Admin: reports', () => {
  test('an admin hides a reported photo: it leaves the leaderboard, and its owner sees why', async ({ page, browser, request }) => {
    const owner = await pq.signUp(request, 'owner');
    const { title, photo } = await competitionWith(request, owner, 'owners-grass.jpg');

    // A photographer reports it from the leaderboard.
    const reporter = await pq.signUp(request, 'reporter');
    const rp = await ui.secondBrowser(browser, reporter);
    await ui.openCompetition(rp, title);
    const entry = tid(rp, 'lb-row').filter({ hasText: nameOf(owner) });
    await entry.getByTestId('report-btn').click();
    await entry.getByTestId('report-reason').selectOption('stolen');
    await entry.getByTestId('report-note').fill('I saw this on someone else’s portfolio');
    await entry.getByTestId('report-submit').click();
    await expect(entry.getByTestId('report-msg')).toHaveText('Reported — an admin will look at it.');
    await entry.getByTestId('report-submit').click();
    await expect(entry.getByTestId('report-msg')).toHaveText('Not reported: you have already reported this photo');

    // The admin sees it in the open queue, with the photo, and hides it.
    await adminInPage(page, request);
    const report = page.locator(`[data-testid=report-row][data-photo="${photo.id}"]`);
    await expect(report).toContainText('stolen: I saw this on someone else’s portfolio');
    await expect(report).toContainText(`owners-grass.jpg by ${owner.email}`);
    await expect(report).toContainText(`reported by ${reporter.email}`);
    await expect.poll(() => report.locator('img.thumb').evaluate((i) => i.complete && i.naturalWidth > 0)).toBe(true);
    await report.getByTestId('hide-btn').click();
    await expect(report.getByTestId('report-action-msg')).toHaveText('A reason is required — the owner is shown it.');
    const reason = 'Reported as taken from another photographer';
    await report.getByTestId('hide-reason').fill(reason);
    await report.getByTestId('hide-btn').click();
    await expect(report).toHaveCount(0); // no longer open
    await tid(page, 'report-state').selectOption('actioned');
    await expect(report).toContainText(`actioned: ${reason}`);
    await expect(report).toContainText('hidden');

    // Gone from the leaderboard for everyone else.
    await ui.openCompetition(rp, title);
    await expect(tid(rp, 'lb-row')).toHaveCount(1);
    await expect(tid(rp, 'lb-row').filter({ hasText: nameOf(owner) })).toHaveCount(0);

    // The owner still has it, marked hidden, with the reason.
    const op = await ui.secondBrowser(browser, owner);
    const tile = op.locator('#gallery .tile', { hasText: 'owners-grass.jpg' });
    await expect(tile).toContainText('hidden');
    await tile.click();
    await expect(tid(op, 'moderation-notice')).toContainText(`Hidden by a moderator: ${reason}`);

    // Unhidden, it is back on the leaderboard.
    await report.getByTestId('unhide-btn').click();
    await expect(report).not.toContainText('hidden');
    await ui.openCompetition(rp, title);
    await expect(tid(rp, 'lb-row').filter({ hasText: nameOf(owner) })).toHaveCount(1);
    await rp.context().close();
    await op.context().close();
  });

  test('an admin dismisses a report, and the photo stays', async ({ page, request }) => {
    const owner = await pq.signUp(request, 'owner');
    const { photo } = await competitionWith(request, owner, 'fine.jpg');
    const reporter = await pq.signUp(request, 'reporter');
    await pq.must(request, reporter.token, 'POST', `/api/photos/${photo.id}/reports`, { reason: 'spam', note: 'looks like an ad' });

    await adminInPage(page, request);
    const report = page.locator(`[data-testid=report-row][data-photo="${photo.id}"]`);
    await report.getByTestId('dismiss-note').fill('A photo of grass, not an ad');
    await report.getByTestId('dismiss-btn').click();
    await expect(report).toHaveCount(0);
    await tid(page, 'report-state').selectOption('dismissed');
    await expect(report).toContainText('dismissed: A photo of grass, not an ad');
    expect((await pq.getPhoto(request, owner.token, photo.id)).body.moderation).toBeUndefined();
  });
});

test.describe('Admin: roles and suspension', () => {
  test('an admin grants curator in the page, and the grantee sees the curator tools without logging in again', async ({ page, browser, request }) => {
    const grantee = await pq.signUp(request, 'grantee');
    const gp = await ui.secondBrowser(browser, grantee);
    await expect(tid(gp, 'nav-curator')).toBeHidden();

    await adminInPage(page, request);
    await tid(page, 'user-filter').fill(grantee.email);
    const row = userRow(page, grantee);
    await expect(row.getByTestId('user-roles')).toHaveText('photographer');
    await row.getByTestId('toggle-curator').click();
    await expect(row.getByTestId('user-roles')).toHaveText(/curator/);
    await expect(row.getByTestId('toggle-curator')).toHaveText('Revoke curator');

    // Same session, same token: the tab appears, and the tools work.
    await expect(tid(gp, 'nav-curator')).toBeVisible({ timeout: 15_000 });
    await ui.nav(gp, 'curator');
    await tid(gp, 'cur-new-journey').click();
    await tid(gp, 'cj-title').fill(`First journey ${tag()}`);
    await tid(gp, 'cj-save').click();
    await expect(tid(gp, 'cj-msg')).toHaveText('Saved.');

    // Revoked: the tab goes away again, and the API says no.
    await row.getByTestId('toggle-curator').click();
    await expect(row.getByTestId('user-roles')).toHaveText('photographer');
    await expect(tid(gp, 'nav-curator')).toBeHidden({ timeout: 15_000 });
    const refused = await pq.call(request, grantee.token, 'GET', '/api/curator/journeys');
    expect(refused.status).toBe(403);
    await gp.context().close();
  });

  test('suspending an account blocks uploading in the page, and unsuspending lifts it', async ({ page, browser, request }) => {
    const member = await pq.signUp(request, 'member');
    const mp = await ui.secondBrowser(browser, member);

    await adminInPage(page, request);
    await tid(page, 'user-filter').fill(member.email);
    const row = userRow(page, member);
    await row.getByTestId('suspend-btn').click();
    await expect(row.getByTestId('user-msg')).toHaveText('A reason is required.');
    await row.getByTestId('suspend-reason').fill('Repeated spam entries');
    await row.getByTestId('suspend-btn').click();
    await expect(row.getByTestId('user-status')).toHaveText('suspended: Repeated spam entries');

    await mp.setInputFiles('#file', SAMPLE.jpeg);
    await mp.click('#upload-btn');
    await expect(mp.locator('#upload-status')).toHaveText('upload failed: your account is suspended');
    // Still signed in, still sees their own page.
    await expect(mp.locator('#gallery')).toContainText('Nothing yet.');

    await row.getByTestId('unsuspend-btn').click();
    await expect(row.getByTestId('user-status')).toHaveText('active');
    await mp.setInputFiles('#file', SAMPLE.jpeg);
    await mp.click('#upload-btn');
    await expect(mp.locator('#upload-status')).toHaveText('evaluated', { timeout: 120_000 });
    await mp.context().close();
  });

  test('admin tools refuse everyone else, and an admin cannot revoke their own admin', async ({ page, request }) => {
    const me = await pq.signUp(request);
    const refused = await pq.call(request, me.token, 'GET', '/api/admin/reports');
    expect(refused.status).toBe(403);
    expect(refused.body.error).toBe('forbidden_role');

    const adm = await adminInPage(page, request);
    await tid(page, 'user-filter').fill(adm.email);
    const row = page.locator(`[data-testid=user-row][data-email="${adm.email}"]`);
    await row.getByTestId('toggle-admin').click();
    await expect(row.getByTestId('user-msg')).toHaveText('Not done: you cannot revoke your own admin role');
    await expect(row.getByTestId('user-roles')).toContainText('admin');
  });
});
