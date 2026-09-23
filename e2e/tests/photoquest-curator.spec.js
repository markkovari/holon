// photoquest, as a curator uses it: build a journey of quests in the page and
// see a photographer receive it; run a timed competition from the page through
// judging to its results.
//
// Run with `bash e2e/photoquest.sh` (all three photoquest specs). The curator
// role is granted through the API by the bootstrap admin (config
// `bootstrap-admin-email`, set by photoquest.sh) — granting it through the admin
// page is photoquest-admin.spec.js's business. Deadlines are passed with
// `POST /test/clock` (config `allow-test-routes`), reset after every test.
//
// Media: only the CC0 samples photoquest.sh downloads (raw.pixls.us), each
// upload salted into bytes of its own (lib/photoquest.js `salted`).

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

/** Fill the requirements form with `prefix` (cq, cc) the way the samples pass. */
async function passableRequirements(page, prefix) {
  if (backend.apple) {
    await tid(page, `${prefix}-subject`).selectOption('label');
    await tid(page, `${prefix}-label`).fill('grass');
    await tid(page, `${prefix}-minconf`).fill('0.5');
  }
  await tid(page, `${prefix}-focus`).fill('5');
  await tid(page, `${prefix}-fnumber`).fill('2.8'); // the ARWs: f/1.2
  await tid(page, `${prefix}-after`).uncheck(); // the samples were taken in 2022
}

test.describe('Curator: journeys and quests', () => {
  test('a curator builds a journey of two quests in the page, and a photographer receives it', async ({ page, browser, request }) => {
    const cur = await pq.curator(request);
    const title = `Park loop ${tag()}`;
    const badge = `Loop closer ${tag()}`;

    await ui.signIn(page, cur);
    await expect(tid(page, 'nav-curator')).toBeVisible();
    await expect(tid(page, 'nav-admin')).toBeHidden();
    await ui.nav(page, 'curator');

    // The journey: title, badge, levels 1 at 0 and 2 at 100 XP.
    await tid(page, 'cur-new-journey').click();
    await tid(page, 'cj-title').fill(title);
    await tid(page, 'cj-description').fill('Two walks through the park.');
    await tid(page, 'cj-badge').fill(badge);
    await tid(page, 'cj-add-level').click();
    await expect(tid(page, 'cj-level-xp')).toHaveCount(2);
    await tid(page, 'cj-level-xp').nth(1).fill('100');
    await tid(page, 'cj-save').click();
    await expect(tid(page, 'cj-msg')).toHaveText('Saved.');
    await expect(tid(page, 'cj-state')).toHaveText('draft');

    // Two quests: made in one order, then reordered.
    for (const [qt, xp] of [['Along the path', 40], ['Into the grass', 70]]) {
      await tid(page, 'cj-new-quest').click();
      await tid(page, 'cq-title').fill(qt);
      await tid(page, 'cq-xp').fill(String(xp));
      await tid(page, 'cq-description').fill(`${qt}, in focus.`);
      await passableRequirements(page, 'cq');
      await tid(page, 'cq-save').click();
      await expect(tid(page, 'cq-msg')).toHaveText('Saved.');
      await tid(page, 'cq-publish').click();
      await expect(tid(page, 'cq-msg')).toHaveText('Published.');
      await expect(tid(page, 'cq-state')).toHaveText('published');
      // A published quest keeps the rules it is judged by.
      await expect(tid(page, 'cq-focus')).toBeDisabled();
    }
    await expect(tid(page, 'cj-quest').locator('strong')).toHaveText(['Along the path', 'Into the grass']);
    await tid(page, 'cj-quest').filter({ hasText: 'Into the grass' }).getByTestId('cj-quest-up').click();
    await expect(tid(page, 'cj-quest').locator('strong')).toHaveText(['Into the grass', 'Along the path']);
    await tid(page, 'cj-publish').click();
    await expect(tid(page, 'cj-msg')).toHaveText('Published — photographers can see it.');
    await expect(tid(page, 'cj-state')).toHaveText('published');

    // A photographer sees it, in the curator's order, and plays it through.
    const me = await pq.signUp(request);
    const photos = [];
    for (let i = 0; i < 2; i++) photos.push(await pq.evaluatedPhoto(request, me.token, SAMPLE.compressed, { salt: true }));
    const player = await ui.secondBrowser(browser, me);
    await ui.openJourney(player, title);
    await expect(tid(player, 'journey-detail')).toContainText('Two walks through the park.');
    await expect(tid(player, 'journey-detail')).toContainText(`finishing it earns the badge “${badge}”`);
    await expect(tid(player, 'quest-title')).toHaveText(['Into the grass', 'Along the path']);
    await expect(ui.questRow(player, 'Into the grass')).toHaveAttribute('data-state', 'open');
    await expect(ui.questRow(player, 'Along the path')).toHaveAttribute('data-state', 'locked');

    await ui.questRow(player, 'Into the grass').click();
    await expect(tid(player, 'quest-requirements')).toContainText('Aperture f/2.8 or wider');
    await ui.submitPhoto(player, photos[0].id);
    await expect(tid(player, 'xp-line')).toHaveText('+70 XP');
    await ui.questRow(player, 'Along the path').click();
    await ui.submitPhoto(player, photos[1].id);
    await expect(tid(player, 'xp-line')).toHaveText('+40 XP');
    await expect(tid(player, 'level-up')).toHaveText(`Level up! ${title}: level 1 → level 2`);
    await expect(tid(player, 'badge-notice')).toHaveText(`Badge earned: ${badge}`);
    await player.context().close();

    // Archiving takes it away from photographers.
    await tid(page, 'cj-archive').click();
    await expect(tid(page, 'cj-state')).toHaveText('archived');
    const list = await pq.must(request, me.token, 'GET', '/api/journeys');
    expect(list.journeys.map((j) => j.title)).not.toContain(title);
  });

  test('curator tools refuse a photographer, and a bad level ladder is refused with a reason', async ({ page, request }) => {
    const me = await pq.signUp(request);
    const refused = await pq.call(request, me.token, 'POST', '/api/curator/journeys', { title: 'Not mine to make' });
    expect(refused.status).toBe(403);
    expect(refused.body.error).toBe('forbidden_role');
    await ui.signIn(page, me);
    await expect(tid(page, 'nav-curator')).toBeHidden();

    const cur = await pq.curator(request);
    await ui.signIn(page, cur);
    await ui.nav(page, 'curator');
    await tid(page, 'cur-new-journey').click();
    await tid(page, 'cj-title').fill(`Upside down ${tag()}`);
    await tid(page, 'cj-add-level').click();
    await tid(page, 'cj-level-xp').nth(1).fill('0');
    await tid(page, 'cj-save').click();
    await expect(tid(page, 'cj-msg')).toHaveText('Not saved: bad_levels: levels[1].xp must be above the level before it');
  });
});

test.describe('Curator: competitions', () => {
  test('a curator runs a competition from the page through judging to its results', async ({ page, browser, request }) => {
    const cur = await pq.curator(request);
    const title = `Green hour ${tag()}`;

    await ui.signIn(page, cur);
    await ui.nav(page, 'curator');
    await tid(page, 'cur-tab-competitions').click();
    await tid(page, 'cc-new').click();
    await tid(page, 'cc-title').fill(title);
    await tid(page, 'cc-brief').fill('Grass, at its greenest.');
    const t = pq.nowSecs();
    await tid(page, 'cc-opens').fill(await ui.localInput(page, t));
    await tid(page, 'cc-closes').fill(await ui.localInput(page, t + 3600));
    await tid(page, 'cc-voting').fill(await ui.localInput(page, t + 2 * 3600));
    await tid(page, 'cc-judging').fill(await ui.localInput(page, t + 3 * 3600));
    await tid(page, 'cc-w-auto').fill('0.5');
    await tid(page, 'cc-w-votes').fill('0');
    await tid(page, 'cc-w-judges').fill('0.5');
    await tid(page, 'cc-prizes').fill('150');
    await tid(page, 'cc-limit').fill('1');
    await passableRequirements(page, 'cc');
    await tid(page, 'cc-save').click();
    await expect(tid(page, 'cc-msg')).toHaveText('Saved.');
    await tid(page, 'cc-publish').click();
    await expect(tid(page, 'cc-msg')).toHaveText('Published — photographers can enter it.');
    await expect(tid(page, 'cc-state')).toHaveText('published');
    await expect(tid(page, 'cc-w-auto')).toBeDisabled();

    // Two photographers enter; the ARW scores higher on auto-v1 than the JPEG.
    const ana = await pq.signUp(request, 'ana');
    const ben = await pq.signUp(request, 'ben');
    const comp = (await pq.must(request, ana.token, 'GET', '/api/competitions')).competitions.find((c) => c.title === title);
    expect(comp.requirements.captured_after_start).toBe(false);
    for (const [u, file] of [[ana, SAMPLE.uncompressed], [ben, SAMPLE.compressed]]) {
      const p = await pq.evaluatedPhoto(request, u.token, file, { salt: true });
      await pq.must(request, u.token, 'POST', `/api/competitions/${comp.id}/entries`, { photo_id: p.id });
    }
    const [anaName, benName] = [ana.email.split('@')[0], ben.email.split('@')[0]];

    // Entries and voting close; the curator judges in the page: ben 9, ana 2.
    await pq.setClock(request, comp.voting_closes_at - pq.nowSecs() + 60);
    await tid(page, 'cur-comp-row').filter({ hasText: title }).getByTestId('cc-judge').click();
    await expect(tid(page, 'judge-panel')).toContainText('judging');
    await expect(tid(page, 'judge-row')).toHaveCount(2);
    for (const [name, score] of [[benName, '9'], [anaName, '2']]) {
      const row = tid(page, 'judge-row').filter({ hasText: name });
      await row.getByTestId('judge-score').fill(score);
      await row.getByTestId('judge-note').fill(`scored ${score}`);
      await row.getByTestId('judge-save').click();
      await expect(tid(page, 'judge-msg')).toHaveText(`Score saved: ${score}.`);
    }
    await expect(tid(page, 'judge-row').filter({ hasText: benName })).toContainText('judges 0.90 ×50% (1 score, mean 9.0/10)');

    // After judging closes, the results: ben first, and paid.
    await pq.setClock(request, comp.judging_closes_at - pq.nowSecs() + 60);
    const player = await ui.secondBrowser(browser, ben);
    await ui.openCompetition(player, title);
    await expect(tid(player, 'competition-phase')).toHaveText('ended');
    await expect(tid(player, 'competition-brief')).toHaveText('Grass, at its greenest.');
    await expect(tid(player, 'winner')).toHaveText([`1st place: ${benName} (you) — 150 XP`]);
    await expect(tid(player, 'result-row').locator('td:nth-child(2)')).toHaveText([`${benName} (you) — winner`, anaName]);
    await ui.nav(player, 'progress');
    await expect(tid(player, 'total-xp')).toHaveText('150 XP');
    await player.context().close();

    // Judging is over: a late score is refused with a reason, in the page.
    await tid(page, 'cur-comp-row').filter({ hasText: title }).getByTestId('cc-judge').click();
    await expect(tid(page, 'judge-panel')).toContainText('ended'); // the fresh panel, not the last one
    const row = tid(page, 'judge-row').filter({ hasText: anaName });
    await row.getByTestId('judge-score').fill('10');
    await row.getByTestId('judge-save').click();
    await expect(tid(page, 'judge-msg')).toHaveText('Not saved: judging has closed');
  });
});
