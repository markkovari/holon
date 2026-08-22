import { test, expect } from '@playwright/test';
import { chromium } from 'playwright';

(async () => {
  const browser = await chromium.launch();
  const context = await browser.newContext({ recordVideo: { dir: 'tools/screencast/videos/freight-tracker' } });
  const page = await context.newPage();
  
  try {
      await page.goto('http://localhost:3055');
      await page.waitForTimeout(1000);
      console.log('Successfully recorded freight-tracker gif.');
  } catch (e) {
      console.error(e);
  } finally {
      await context.close();
      await browser.close();
  }
})();
