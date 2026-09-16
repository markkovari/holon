const { test, expect } = require('@playwright/test');

test('Social Photo Feed Flow', async ({ page }) => {
  // 1. Visit the app
  await page.goto('http://127.0.0.1:3000');

  // The app will silently register/login via init()
  // Wait for the user to be logged in
  await expect(page.locator('#userName')).not.toHaveText('Guest', { timeout: 10000 });
  await expect(page.locator('#userName')).toContainText('usr_');

  // 2. Upload a new photo
  await page.click('#uploadBtn');
  await expect(page.locator('#uploadModal')).toBeVisible();
  
  const testTitle = 'E2E Automated Photo ' + Date.now();
  await page.fill('#uploadTitle', testTitle);
  await page.fill('#uploadDesc', 'This is an automated E2E photo description.');
  
  await page.click('#uploadSubmitBtn');
  
  // Wait for upload to complete
  await expect(page.locator('#uploadModal')).not.toBeVisible({ timeout: 10000 });
  
  // Verify the new photo appears in the feed
  const newPhotoCard = page.locator('.photo-card').filter({ hasText: testTitle }).first();
  await expect(newPhotoCard).toBeVisible();

  // 4. Vote on the photo
  const upvoteBtn = newPhotoCard.locator('.vote-btn').first();
  await upvoteBtn.click();
  
  // The score should become 1
  const scoreSpan = newPhotoCard.locator('.score-count');
  await expect(scoreSpan).toHaveText('1');

  // 5. Open Photo Details and Rate Attributes
  await newPhotoCard.locator('.photo-title').click();
  await expect(page.locator('#photoModal')).toBeVisible();
  
  // Wait for AI critique to populate (might take a second)
  await expect(page.locator('#modalAiNarrative')).not.toBeEmpty();

  // Adjust a rating slider
  const slider = page.locator('.rating-slider').first();
  await slider.fill('9.5');
  
  // Submit ratings
  await page.click('button:has-text("Submit Ratings")');
  await expect(page.locator('#photoModal')).not.toBeVisible();
  
  console.log('App 6 E2E Test passed!');
});
