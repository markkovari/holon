#!/usr/bin/env bash
set -e

# Build host if needed
cargo build --release --manifest-path ../../../host/Cargo.toml

echo "Cleaning up old database..."
rm -f comp-kv.db*

# Start the host in the background
echo "Starting backend..."
cargo run --release --manifest-path ../../../host/Cargo.toml --bin comp-host -- --app ticket-triage-domain --component ../../target/ticket-triage-domain.composed.wasm --addr 127.0.0.1:3000 --kv sqlite --config "default-tenant=tickettriage" &
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
