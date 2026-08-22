import { test, expect } from '@playwright/test';
import { startRecording, stopRecording } from './screencast-utils.js';

test('Image Optimizer showcase recording', async ({ page }) => {
    await startRecording(page, 'image-optimizer.gif');
    
    // Navigate to the domain
    await page.goto('http://localhost:3056/');
    await expect(page.locator('h1')).toContainText('Image Optimizer');
    
    // Trigger the native capability
    await page.click('button');
    
    // Wait for the backend native capability to mock a response
    await page.waitForTimeout(1000);
    
    await stopRecording(page);
});
