const { test, expect } = require('@playwright/test');

test('Volunteer Shift Flow', async ({ page }) => {
  // 1. Log in as coordinator
  await page.goto('http://127.0.0.1:3000/');
  await page.locator('#login-coord').click();
  await expect(page.locator('.toast').last()).toHaveText(/Authenticated as coordinator/);
  
  // 2. Create a shift with 2 slots
  await page.locator('#shift-title').fill('Beach Cleanup');
  await page.locator('#shift-slots').fill('2');
  await page.locator('button:has-text("Create Shift")').click();
  await expect(page.locator('.toast').last()).toHaveText('Shift created successfully!');
  
  // Verify it's on the page
  const shiftCards = page.locator('.shift-card');
  await expect(shiftCards.first()).toBeVisible();
  await expect(shiftCards.first().locator('.shift-title')).toHaveText('Beach Cleanup');
  
  // 3. Logout
  await page.locator('#logout-btn').click();
  
  // 4. Log in as volunteer A
  await page.locator('#login-vol-a').click();
  await expect(page.locator('.toast').last()).toHaveText(/Authenticated as volunteer/);
  
  // 5. Sign up for the shift
  const firstShift = shiftCards.first();
  await expect(firstShift.locator('.signup-btn')).toBeVisible();
  await firstShift.locator('.signup-btn').click();
  
  await expect(page.locator('.toast').last()).toHaveText('Signed up successfully!');
  
  // Verify that the cancel button is now visible
  await expect(firstShift.locator('.cancel-btn')).toBeVisible();
  await expect(firstShift.locator('.signup-btn')).toBeHidden();
  
  // Verify slots count is 1 / 2
  await expect(firstShift.locator('.shift-slots')).toHaveText('Slots: 1 / 2');
  
  // 6. Logout
  await page.locator('#logout-btn').click();
  
  // 7. Log in as volunteer B
  await page.locator('#login-vol-b').click();
  
  // 8. Sign up for the same shift
  await expect(firstShift.locator('.signup-btn')).toBeVisible();
  await firstShift.locator('.signup-btn').click();
  await expect(page.locator('.toast').last()).toHaveText('Signed up successfully!');
  
  // Verify slots count is 2 / 2
  await expect(firstShift.locator('.shift-slots')).toHaveText('Slots: 2 / 2');
  
  // 9. Try to cancel (vol-b can cancel their own)
  await firstShift.locator('.cancel-btn').click();
  await expect(page.locator('.toast').last()).toHaveText('Signup cancelled!');
  
  // Verify slots count is 1 / 2 again
  await expect(firstShift.locator('.shift-slots')).toHaveText('Slots: 1 / 2');
  
  // 10. Logout
  await page.locator('#logout-btn').click();
});
