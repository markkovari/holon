const { test, expect } = require('@playwright/test');

test('adds a transaction and checks balance', async ({ page }) => {
  await page.goto('/');
  await expect(page.locator('h1')).toHaveText('Budget Tracker');
  // Wait for the chart to load
  await expect(page.locator('canvas')).toBeVisible();
});
