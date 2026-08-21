const { defineConfig } = require('@playwright/test');

module.exports = defineConfig({
  testDir: './',
  use: {
    baseURL: 'http://localhost:5173',
  },
});
