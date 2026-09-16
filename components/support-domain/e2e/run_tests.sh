#!/usr/bin/env bash
set -e

echo "Building WASM components..."
(cd ../../../ && cargo component build --release --manifest-path components/Cargo.toml --target-dir components/target --target wasm32-wasip2 -p support-domain -p auth-guard -p rate-limiter -p audit-log -p policy-guard -p record-store -p id-generate -p ai-inference -p mock-provider)
rm -f ../../../components/target/wasm32-wasip1/release/anthropic_provider.wasm
rm -f ../../../components/target/wasm32-wasip1/release/openai_provider.wasm
rm -f ../../../components/target/composed/support-domain.*.wasm

echo "Composing WASM components..."
ART=$(cd ../../../ && ./reconciler/target/release/comp-plug support-domain)
echo "Composed to ${ART}"
echo "Cleaning up old database..."
rm -f comp-kv.db*

# Start the host in the background
echo "Starting backend..."
cargo run --release --manifest-path ../../../host/Cargo.toml --bin comp-host -- --app support-domain --component "${ART}" --addr 127.0.0.1:3000 --kv sqlite --config "default-tenant=support" --config "mock-script={\"rules\":[{\"when\":\"*\",\"text\":\"Please try resetting your password.\"}]}" &
HOST_PID=$!
trap "kill -9 $HOST_PID" EXIT

# Wait for the backend to start
sleep 2

# Run Playwright tests
echo "Running Playwright tests..."
if [ ! -d "node_modules" ]; then
    npm init -y
    npm install @playwright/test
    npx playwright install chromium
fi
npx playwright test

# Kill the host
echo "Shutting down backend..."
