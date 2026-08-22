import { test, expect } from '@playwright/test';
import { chromium } from 'playwright';

(async () => {
  const browser = await chromium.launch();
  const context = await browser.newContext({ recordVideo: { dir: 'tools/screencast/videos/real-estate-escrow' } });
  const page = await context.newPage();
  
  try {
      await page.goto('http://localhost:3055');
      await page.waitForTimeout(1000);
      console.log('Successfully recorded real-estate-escrow gif.');
  } catch (e) {
      console.error(e);
  } finally {
      await context.close();
      await browser.close();
  }
})();
