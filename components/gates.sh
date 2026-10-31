#!/usr/bin/env bash
# Runs all component gates via native Rust integration tests (ADR-0087 / ADR-0088).
# Fast, in-process capability graph composition, ephemeral OS port binding, and parallel execution.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo test --release -p holon-reconciler --test gate_scaffolds --test gate_bytes_codec "$@"
