const { test, expect } = require('@playwright/test');

test('E-Commerce Fulfillment Flow', async ({ page, context }) => {
  // Wait for the app to be responsive
  await page.goto('http://127.0.0.1:3000/');
  
  // 1. Customer creates an order
  await page.selectOption('#role-select', 'customer');
  await page.click('#login-btn');
  
  await expect(page.locator('#toast-container')).toContainText('Authenticated as customer');
  await expect(page.locator('#customer-section')).toBeVisible();
  
  // Create an order
  await page.fill('#qty-input', '2');
  await page.click('#buy-btn');
  
  // Wait for toast indicating successful purchase
  await expect(page.locator('#toast-container')).toContainText('Order');
  await expect(page.locator('#toast-container')).toContainText('placed successfully!');
  
  // 2. Logout customer
  await page.click('#logout-btn');
  await expect(page.locator('#auth-view')).toBeVisible();
  
  // 3. Fulfillment staff logs in
  await page.selectOption('#role-select', 'fulfillment');
  await page.click('#login-btn');
  
  await expect(page.locator('#toast-container')).toContainText('Authenticated as fulfillment');
  await expect(page.locator('#fulfillment-section')).toBeVisible();
  
  // Check that the order is visible and has a "paid" status
  const orderCards = page.locator('.order-card');
  await expect(orderCards.first()).toBeVisible();
  await expect(orderCards.first().locator('.status')).toHaveText('paid');
  
  // 4. Ship the order
  await orderCards.first().locator('.fulfill-btn').click();
  
  // Validate shipped status
  await expect(page.locator('#toast-container')).toContainText('shipped!');
  await expect(orderCards.first().locator('.status')).toHaveText('shipped');
  await expect(orderCards.first().locator('.fulfill-btn')).toHaveCount(0); // button should be gone
});
