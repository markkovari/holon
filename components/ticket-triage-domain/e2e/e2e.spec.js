const { test, expect } = require('@playwright/test');

test('Ticket Triage Flow', async ({ page }) => {
  // 1. Log in as billing agent
  await page.goto('http://127.0.0.1:3000/');
  await page.locator('#login-agent-billing').click();
  await expect(page.locator('.toast').last()).toHaveText(/Authenticated as agent-b/);
  
  // 2. Create a ticket in the billing queue
  await page.locator('#ticket-subject').fill('double charge');
  await page.locator('#ticket-body').fill('charged twice for the same order');
  await page.locator('#ticket-queue').selectOption('billing');
  await page.locator('button:has-text("Create Ticket")').click();
  
  await expect(page.locator('.toast').last()).toHaveText('Ticket created successfully!');
  
  // 3. Search for the ticket
  await page.locator('#search-query').fill('charge');
  await page.locator('#search-queue').selectOption('billing');
  await page.locator('button:has-text("Search")').click();
  
  // Verify it appears in the results
  const ticketCards = page.locator('.ticket-card');
  await expect(ticketCards).toHaveCount(1);
  await expect(ticketCards.first().locator('.ticket-subject')).toHaveText('double charge');
  
  // 4. Logout
  await page.locator('#logout-btn').click();
  
  // 5. Log in as search agent (different queue)
  await page.locator('#login-agent-search').click();
  
  // 6. Search for the same ticket (billing queue)
  await page.locator('#search-query').fill('charge');
  await page.locator('#search-queue').selectOption('billing');
  await page.locator('button:has-text("Search")').click();
  
  // Verify it appears
  await expect(ticketCards).toHaveCount(1);
  
  // 7. Try to resolve it (should fail due to policy:guard)
  await ticketCards.first().locator('.resolve-btn').click();
  await expect(page.locator('.toast').last()).toHaveText(/Access Denied/);
  
  // 8. Logout
  await page.locator('#logout-btn').click();
  
  // 9. Log in as billing agent again
  await page.locator('#login-agent-billing').click();
  
  // 10. Search for the ticket
  await page.locator('#search-query').fill('charge');
  await page.locator('#search-queue').selectOption('billing');
  await page.locator('button:has-text("Search")').click();
  
  // 11. Resolve it
  await ticketCards.first().locator('.resolve-btn').click();
  await expect(page.locator('.toast').last()).toHaveText('Ticket resolved!');
  
  // Verify it disappears from search results
  // The app.js code automatically triggers a search on successful resolution,
  // so the list should be empty or say "No tickets found."
  await expect(page.locator('#tickets-list')).toHaveText(/No tickets found/);
});
