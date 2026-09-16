const { test, expect } = require('@playwright/test');

test('Book Lending Flow', async ({ page }) => {
  page.on('console', msg => {
      console.log('BROWSER LOG:', msg.text());
  });
  // 1. Log in as Alice (Patron)
  await page.goto('http://127.0.0.1:3000/');
  await page.locator('#login-alice').click();
  await expect(page.locator('.toast').last()).toHaveText(/Authenticated as alice/);
  
  // Alice cannot add a book (UI is hidden, we can't test unless we try API, but we skip it here since Hurl tests it)
  // 2. Logout Alice
  await page.locator('#logout-btn').click();
  
  // 3. Log in as Librarian
  await page.locator('#login-lib').click();
  await expect(page.locator('.toast').last()).toHaveText(/Authenticated as lib/);
  
  // 4. Add a book
  await page.locator('#book-title').fill('The Left Hand of Darkness');
  await page.locator('button:has-text("Add to Catalog")').click();
  
  await expect(page.locator('.toast').last()).toHaveText('Book added successfully!');
  
  const bookCards = page.locator('.book-card');
  await expect(bookCards).toHaveCount(1);
  await expect(bookCards.first().locator('.book-title')).toHaveText('The Left Hand of Darkness');
  await expect(bookCards.first().locator('.book-callnum')).toContainText('BK-');
  
  // 5. Logout Librarian
  await page.locator('#logout-btn').click();
  
  // 6. Log in as Alice (Patron)
  await page.locator('#login-alice').click();
  
  // 7. Borrow the book
  await expect(bookCards).toHaveCount(1);
  await bookCards.first().locator('.borrow-btn').click();
  
  await expect(page.locator('.toast').last()).toHaveText('Book borrowed!');
  try {
    await expect(page.locator('.status-borrowed')).toBeVisible({ timeout: 5000 });
  } catch (e) {
    const html = await page.locator('#books-list').innerHTML();
    const ls = await page.evaluate(() => JSON.stringify(Object.assign({}, window.localStorage)));
    console.log("HTML OF BOOKS LIST: ", html);
    console.log("LOCAL STORAGE: ", ls);
    throw e;
  }
  
  // 8. Logout Alice
  await page.locator('#logout-btn').click();
  
  // 9. Log in as Bob (Patron)
  await page.locator('#login-bob').click();
  
  // 10. Borrow the same book
  await expect(page.locator('.borrow-btn')).toBeHidden();
  await expect(page.locator('.return-btn')).toBeVisible(); // Bob sees return button because it's borrowed
  
  // 11. Try to return it (should fail policy:guard)
  await page.locator('.return-btn').click();
  await expect(page.locator('.toast').last()).toHaveText(/Access Denied/);
  
  // 12. Logout Bob
  await page.locator('#logout-btn').click();
  
  // 13. Log in as Alice
  await page.locator('#login-alice').click();
  await page.locator('.return-btn').click();
  await expect(page.locator('.toast').last()).toHaveText('Book returned successfully!');
  
  // Verify it's available again
  await expect(page.locator('.status-available')).toBeVisible();
  await expect(page.locator('.borrow-btn')).toBeVisible();
  
  // 15. Logout Alice
  await page.locator('#logout-btn').click();
  
  // 16. Log in as Bob
  await page.locator('#login-bob').click();
  
  // 17. Borrow the book
  await page.locator('.borrow-btn').click();
  await expect(page.locator('.toast').last()).toHaveText('Book borrowed!');
  
  // 18. Logout Bob
  await page.locator('#logout-btn').click();
  
  // 19. Log in as Librarian
  await page.locator('#login-lib').click();
  
  await page.locator('.return-btn').click();
  await expect(page.locator('.toast').last()).toHaveText('Book returned successfully!');
  await expect(page.locator('.status-available')).toBeVisible();
});
