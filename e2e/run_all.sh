#!/usr/bin/env bash
set -e

APPS=(
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

# Build host if needed
cargo build --release --manifest-path ../host/Cargo.toml

for APP in "${APPS[@]}"; do
  echo "=========================================="
  echo "Running E2E tests for ${APP}"
  echo "=========================================="

  APP_NAME=${APP%-domain}

  echo "Building WASM components..."
  (cd ../ && cargo component build --release --manifest-path components/Cargo.toml --target-dir components/target --target wasm32-wasip2 -p ${APP} -p auth-guard -p rate-limiter -p audit-log -p policy-guard -p record-store -p id-generate)

  echo "Composing WASM components..."
  ART=$(cd ../ && ./reconciler/target/release/comp-plug ${APP})

  echo "Cleaning up old database..."
  rm -f comp-kv.db*

  echo "Starting backend..."
  cargo run --release --manifest-path ../host/Cargo.toml --bin comp-host -- --app ${APP} --component "${ART}" --addr 127.0.0.1:3000 --kv sqlite --config "default-tenant=${APP_NAME}" &
  HOST_PID=$!
  
  # Allow the backend to start up
  sleep 2

  echo "Running Playwright tests for ${APP_NAME}..."
  npx playwright test tests/${APP_NAME}.spec.js || {
    echo "Tests failed for ${APP}"
    kill -9 $HOST_PID
    exit 1
  }

  echo "Shutting down backend..."
  kill -9 $HOST_PID
  
  # Wait for process to fully terminate
  sleep 1
done

echo "All tests completed successfully!"
