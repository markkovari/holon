#!/usr/bin/env bash
set -e

# Build host if needed
cargo build --release --manifest-path ../../../host/Cargo.toml

echo "Building WASM components..."
(cd ../../../ && cargo component build --release --manifest-path components/Cargo.toml --target-dir components/target --target wasm32-wasip2 -p book-lending-domain -p auth-guard -p rate-limiter -p audit-log -p policy-guard -p record-store -p id-generate)

echo "Composing WASM components..."
ART=$(cd ../../../ && ./reconciler/target/release/comp-plug book-lending-domain)

echo "Cleaning up old database..."
rm -f comp-kv.db*

# Start the host in the background
echo "Starting backend..."
cargo run --release --manifest-path ../../../host/Cargo.toml --bin comp-host -- --app book-lending-domain --component "${ART}" --addr 127.0.0.1:3000 --kv sqlite --config "default-tenant=booklending" &
HOST_PID=$!
trap "kill -9 $HOST_PID" EXIT

# Wait for the backend to start
sleep 2

# Run Playwright tests
echo "Running Playwright tests..."
npx playwright test

# Kill the host
echo "Shutting down backend..."
# The trap will handle killing the process
