const { test, expect } = require('@playwright/test');

test('Social Moderation Flow', async ({ page }) => {
  page.on('dialog', dialog => console.log('DIALOG:', dialog.message()));

  // 1. Visit the app
  await page.goto('http://127.0.0.1:3000');
  
  // 2. Submit a new item for moderation
  const testText = 'Playwright Test Content ' + Date.now();
  await page.fill('#intakeText', testText);
  await page.click('button:has-text("Submit Content")');
  
  // Wait for it to appear in the queue
  const queueItem = page.locator('.queue-item').filter({ hasText: testText });
  await expect(queueItem).toBeVisible({ timeout: 5000 });
  
  // 3. Select the item for review
  await queueItem.click();
  
  // Wait for review panel to show
  await expect(page.locator('#reviewPanel')).toContainText(testText);
  
  // 4. Trigger the review
  await page.click('button:has-text("Run Automated Review")');
  
  // Wait for it to disappear from the queue
  await expect(queueItem).not.toBeVisible({ timeout: 5000 });
  
  console.log('App 9 E2E Test passed!');
});
