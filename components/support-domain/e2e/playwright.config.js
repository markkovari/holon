const { defineConfig } = require('@playwright/test');
module.exports = defineConfig({
  use: {
    video: 'on',
    baseURL: 'http://127.0.0.1:3000',
  },
});
