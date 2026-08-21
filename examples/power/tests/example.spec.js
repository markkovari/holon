import { test, expect } from '@playwright/test';

test('calculate power cost', async ({ page }) => {
  await page.goto('http://localhost:5173');
  
  // Login first
  await page.fill('input[type="email"]', 'test@example.com');
  await page.fill('input[type="password"]', 'password123');
  await page.click('[data-testid="login-button"]');

  // Now calculate
  await page.fill('[data-testid="wattage-input"]', '1000');
  await page.fill('[data-testid="hours-input"]', '2');
  await page.click('[data-testid="calculate-button"]');
  await expect(page.locator('[data-testid="cost-result"]')).toHaveText('$0.30');
});
