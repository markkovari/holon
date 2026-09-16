const { test, expect } = require('@playwright/test');

test('Smart Home Flow', async ({ page }) => {
  // Catch console logs for debugging
  page.on('console', msg => console.log('BROWSER LOG:', msg.text()));
  
  await page.goto('http://127.0.0.1:3000/');
  
  // 1. Log in as Admin
  await page.locator('#login-admin').click();
  await expect(page.locator('.toast').last()).toHaveText(/Authenticated as usr_/);
  
  // 2. Dashboard should be visible
  await expect(page.locator('#dashboard-section')).toBeVisible();
  
  // 3. Create a device
  await page.locator('#new-device-name').fill('Living Room Light');
  await page.locator('#create-device-form button[type="submit"]').click();
  
  await expect(page.locator('.toast').last()).toHaveText('Device added successfully!');
  
  // 4. Device should appear and be OFF
  const deviceCard = page.locator('.device-card').first();
  await expect(deviceCard.locator('.device-name')).toHaveText('Living Room Light');
  await expect(deviceCard.locator('.device-status')).toHaveText('OFF');
  await expect(deviceCard).not.toHaveClass(/is-on/);
  
  // 5. Toggle device ON
  await deviceCard.click();
  await expect(page.locator('.toast').last()).toHaveText('Device turned ON');
  await expect(deviceCard.locator('.device-status')).toHaveText('ON');
  await expect(deviceCard).toHaveClass(/is-on/);
  
  // 6. Toggle device OFF
  await deviceCard.click();
  await expect(page.locator('.toast').last()).toHaveText('Device turned OFF');
  await expect(deviceCard.locator('.device-status')).toHaveText('OFF');
  await expect(deviceCard).not.toHaveClass(/is-on/);
  
  // 7. Logout
  await page.locator('#logout-btn').click();
  await expect(page.locator('#auth-section')).toBeVisible();
  
  // 8. Log in as User
  await page.locator('#login-user').click();
  await expect(page.locator('.toast').last()).toHaveText(/Authenticated as usr_/);
  
  // User should see create device controls
  await expect(page.locator('#admin-controls')).toBeVisible();
  
  // User should not see user 1's devices
  await expect(page.locator('#devices-grid')).toHaveText(/No devices found./);
});
