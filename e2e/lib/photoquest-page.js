// Page helpers shared by the photoquest specs (photoquest, -curator, -admin):
// the few things every scenario does through the page itself — register, log
// in, switch tabs, open a journey or a competition by its title. Everything a
// scenario only needs as a precondition is set up through lib/photoquest.js.

const { expect } = require('@playwright/test');
const pq = require('./photoquest');

async function registerInPage(page, who = 'photographer') {
  const email = pq.uniqueEmail(who), password = 'correct horse battery';
  await page.goto(pq.BASE);
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

/** Open the app and sign in as `user` (anything with email and password),
 *  logging out whoever this page was signed in as. */
async function signIn(page, user) {
  await page.goto(pq.BASE);
  if (await page.evaluate(() => !!localStorage.getItem('token'))) {
    await page.click('#logout-btn');
  }
  await expect(page.locator('#auth')).toBeVisible();
  await logInInPage(page, user);
  await expect(page.locator('#app')).toBeVisible();
}

/** A second browser, signed in as `user`: its own context, so its own token. */
async function secondBrowser(browser, user) {
  const context = await browser.newContext();
  const page = await context.newPage();
  await signIn(page, user);
  return page;
}

const tid = (page, id) => page.getByTestId(id);

async function nav(page, view) {
  await tid(page, `nav-${view}`).click();
}

async function openJourney(page, title) {
  await nav(page, 'journeys');
  await tid(page, 'journey-card').filter({ hasText: title }).click();
  await expect(tid(page, 'journey-detail')).toContainText(title);
}

/** A quest row by its exact title (a locked row names the quest before it too). */
function questRow(page, title) {
  return tid(page, 'quest-row').filter({ has: page.getByTestId('quest-title').getByText(title, { exact: true }) });
}

async function openQuest(page, journeyTitle, questTitle) {
  await openJourney(page, journeyTitle);
  await questRow(page, questTitle).click();
  await expect(tid(page, 'quest-detail')).toContainText(questTitle);
}

/** Submit `photoId` to the quest on the page; returns the verdict locator. */
async function submitPhoto(page, photoId) {
  await tid(page, 'submit-photo-select').selectOption(photoId);
  await tid(page, 'submit-photo-btn').click();
  const verdict = tid(page, 'verdict');
  await expect(verdict).not.toBeEmpty();
  return verdict;
}

async function openCompetition(page, title) {
  await nav(page, 'competitions');
  await tid(page, 'competition-card').filter({ hasText: title }).click();
  await expect(tid(page, 'competition-detail')).toContainText(title);
}

/** The page's own date format (app.js `fmtTime`), in the browser's time zone. */
function fmtTime(page, secs) {
  return page.evaluate((s) => fmtTime(s), secs); // eslint-disable-line no-undef
}

/** `<input type="datetime-local">` value for unix seconds, in the browser's zone. */
function localInput(page, secs) {
  return page.evaluate((s) => toLocalInput(s), secs); // eslint-disable-line no-undef
}

/** One check line of a verdict, by the requirement's name. */
function check(scope, name) {
  return scope.locator(`[data-testid=check][data-name="${name}"]`);
}

module.exports = {
  registerInPage, logInInPage, signIn, secondBrowser, tid, nav,
  openJourney, questRow, openQuest, submitPhoto, openCompetition, fmtTime, localInput, check,
};
