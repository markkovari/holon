const { test, expect } = require('@playwright/test');

test('CI/CD Orchestrator Flow', async ({ page }) => {
  // 1. Visit the app
  await page.goto('http://127.0.0.1:3000');
  
  // Wait for SSE connection and stats to initialize to 0
  await expect(page.locator('#statPending')).toHaveText('0');
  
  // 2. Enqueue an event while the sink is UP
  await page.click('#enqueueBtn');
  await expect(page.locator('#enqueueModal')).toBeVisible();
  
  await page.fill('#eventTopic', 'test.event.up');
  await page.click('button:has-text("Submit Event")');
  await expect(page.locator('#enqueueModal')).not.toBeVisible();
  
  // It should quickly transition to enqueued, in-flight, acked
  await expect(page.locator('.log-window#eventLog')).toContainText('test.event.up');
  await expect(page.locator('.log-window#eventLog')).toContainText('acked');
  
  // 3. Toggle Sink DOWN
  await page.click('.slider'); // Toggle the switch
  await expect(page.locator('#sinkStatusText')).toHaveText('DOWN');
  
  // 4. Enqueue an event while the sink is DOWN
  await page.click('#enqueueBtn');
  await page.fill('#eventTopic', 'test.event.down');
  await page.click('button:has-text("Submit Event")');
  
  // Wait for it to hit 'retry' or 'dead'
  await expect(page.locator('.log-window#eventLog')).toContainText('test.event.down');
  await expect(page.locator('.log-window#eventLog')).toContainText('dead', { timeout: 15000 });
  
  // 5. Verify it appears in the Dead Letters list
  await expect(page.locator('#deadLettersList')).toContainText('test.event.down');
  
  // 6. Toggle Sink back UP
  await page.click('.slider');
  await expect(page.locator('#sinkStatusText')).toHaveText('UP');
  
  // 7. Replay the dead letter
  await page.click('button:has-text("↻ Replay Event")');
  
  // Wait for it to be acked
  await expect(page.locator('.log-window#eventLog').first()).toContainText('test.event.down');
  await expect(page.locator('.log-window#eventLog').first()).toContainText('acked', { timeout: 10000 });
  
  console.log('App 7 E2E Test passed!');
});
