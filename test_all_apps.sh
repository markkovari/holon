#!/bin/bash
set -e

echo "Running tests for all apps and generating GIFs..."
cd components
mkdir -p ../gifs

apps=(
  "book-lending-domain"
  "ecommerce-fulfillment-domain"
  "moderation-domain"
  "photosocial-domain"
  "pipeline-domain"
  "real-estate-escrow-domain"
  "smart-home-domain"
  "support-domain"
  "ticket-triage-domain"
  "volunteer-shift-domain"
)

for app in "${apps[@]}"; do
  echo "----------------------------------------"
  echo "Testing App: $app"
  cd "$app/e2e"
  
  cat << 'CFG' > playwright.config.js
const { defineConfig } = require('@playwright/test');
module.exports = defineConfig({
  use: {
    video: 'on',
  },
});
CFG

  ./run_tests.sh
  
  echo "Converting WebM to GIF..."
  WEBM_FILE=$(find test-results -name "*.webm" | head -n 1)
  if [ -n "$WEBM_FILE" ]; then
      ffmpeg -y -i "$WEBM_FILE" -vf "fps=10,scale=800:-1:flags=lanczos,split[s0][s1];[s0]palettegen[p];[s1][p]paletteuse" "../../../gifs/${app}.gif"
      echo "Created GIF for $app"
  fi

  cd ../../
done

echo "All apps tested successfully! GIFs are in the /gifs folder."
