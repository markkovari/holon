import { test, expect } from '@playwright/test';

test('has title', async ({ page }) => {
  await page.goto('/');
  await expect(page).toHaveTitle(/Vite/);
});

test('login and view thread', async ({ page }) => {
  await page.goto('/');
  await page.fill('input[placeholder="Username"]', 'testuser');
  await page.click('button:has-text("Log In")');
  
  await expect(page.locator('text=Logged in as testuser')).toBeVisible();
  await expect(page.locator('text=This is a fully styled mockup thread!')).toBeVisible();
});
