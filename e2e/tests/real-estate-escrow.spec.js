const { test, expect } = require('@playwright/test');

test('Real Estate Escrow Flow', async ({ page }) => {
  // 1. Visit the app
  await page.goto('http://127.0.0.1:3000');
  
  // Wait for auth modal to appear since we are not logged in
  await expect(page.locator('#authOverlay')).toBeVisible();
  
  // 2. Register & Login as Agent
  await page.fill('#authEmail', 'agent@holon.test');
  await page.fill('#authPassword', 'agent1234');
  // Role 'agent' is selected by default in the hidden select
  
  // The logic falls back to register -> login automatically
  await page.click('button:has-text("Register")');
  
  // Wait for auth modal to close
  await expect(page.locator('#authOverlay')).not.toBeVisible();
  await expect(page.locator('#userName')).toContainText('usr_');

  // 3. Create a new escrow transaction
  await page.click('#createTxBtn');
  await expect(page.locator('#createModal')).toBeVisible();
  
  const testTitle = 'E2E Escrow ' + Date.now();
  await page.fill('#txName', testTitle);
  
  await page.click('#submitTxBtn');
  
  // Wait for modal to close
  await expect(page.locator('#createModal')).not.toBeVisible();
  
  // Verify it appears in the grid
  const txCard = page.locator('.tx-card').filter({ hasText: testTitle }).first();
  await expect(txCard).toBeVisible();
  
  console.log('App 8 E2E Test passed!');
});
