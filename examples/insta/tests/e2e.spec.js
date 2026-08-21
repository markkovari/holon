import { test, expect } from '@playwright/test';

test('create post and like it in feed', async ({ page }) => {
  await page.route('/api/posts', async route => {
    if (route.request().method() === 'GET') {
      await route.fulfill({ json: [] });
    } else if (route.request().method() === 'POST') {
      await route.fulfill({
        json: {
          id: 'post_123',
          author_id: 'test_user',
          image_url: 'https://via.placeholder.com/150',
          caption: 'Loving this new clone! 🚀 #insta',
          likes: []
        }
      });
    }
  });

  await page.route('/api/posts/*/like', async route => {
    await route.fulfill({
      json: {
        id: 'post_123',
        author_id: 'test_user',
        image_url: 'https://via.placeholder.com/150',
        caption: 'Loving this new clone! 🚀 #insta',
        likes: ['test_user']
      }
    });
  });

  await page.goto('http://localhost:5173');
  
  // Login
  await expect(page.locator('h2')).toHaveText('Instaclone');
  await page.locator('input[name="username"]').fill('test_user');
  await page.locator('button[type="submit"]').click();
  
  // Feed view
  await expect(page.locator('nav span.font-serif')).toHaveText('Instaclone');
  
  await page.getByTestId('create-post-btn').click();
  
  await expect(page.locator('.post-container')).toHaveCount(1);
  await expect(page.locator('.like-count')).toHaveText('0');
  
  await page.locator('.like-btn').click();
  await expect(page.locator('.like-count')).toHaveText('1');
});
