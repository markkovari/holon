import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: '.',
  use: {
    baseURL: 'http://localhost:5173',
  },
});
