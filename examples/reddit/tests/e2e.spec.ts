import { test, expect } from '@playwright/test';

test('has title', async ({ page }) => {
  await page.goto('/');
  await expect(page).toHaveTitle(/Reddit Clone/);
});

test('post and comment', async ({ page }) => {
  await page.goto('/');
  // Create a subreddit
  // Post a thread
  // Comment on the thread
});
