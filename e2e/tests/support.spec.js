const { test, expect } = require('@playwright/test');

test('Support Desk Flow', async ({ page }) => {
  page.on('console', msg => console.log('BROWSER CONSOLE:', msg.text()));
  page.on('pageerror', err => console.log('BROWSER ERROR:', err.message));
  page.on('dialog', dialog => console.log('DIALOG:', dialog.message()));
  page.on('response', response => console.log('RESPONSE:', response.url(), response.status()));

  await page.goto('http://127.0.0.1:3000');
  
  // 1. Create a ticket
  await page.click('text=New Ticket');
  await expect(page.locator('#createModal')).toHaveClass(/active/);
  
  const title = `Playwright Test Ticket ${Date.now()}`;
  await page.fill('#newTitle', title);
  await page.fill('#newDescription', 'I have a problem with my account.');
  await page.click('text=Submit');
  
  // Wait for modal to close
  await expect(page.locator('#createModal')).not.toHaveClass(/active/);
  
  // 2. Select ticket in list
  const ticketItem = page.locator('.ticket-item', { hasText: title });
  await expect(ticketItem).toBeVisible();
  await ticketItem.click();
  
  // Wait for details to load
  await expect(page.locator('.ticket-details', { hasText: title })).toBeVisible();
  
  // 3. AI Suggestion
  await page.click('text=✨ AI Suggest Reply');
  
  // Wait for the textarea to be populated by the AI
  await expect(page.locator('#replyText')).not.toBeEmpty({ timeout: 10000 });
  const suggestion = await page.locator('#replyText').inputValue();
  console.log('AI Suggestion:', suggestion);
  expect(suggestion.length).toBeGreaterThan(0);
  
  // 4. Send reply
  await page.click('text=Send Reply');
  
  // Verify reply is rendered
  await expect(page.locator('.reply-body', { hasText: suggestion })).toBeVisible();
  
  // 5. Close Ticket
  await page.click('text=Close Ticket');
  
  // Verify ticket is closed in the list
  const closedBadge = ticketItem.locator('.status-closed');
  await expect(closedBadge).toBeVisible();
  
  console.log('App 10 E2E Test passed!');
});
