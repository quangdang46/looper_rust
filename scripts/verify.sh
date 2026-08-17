#!/bin/bash
set -e

echo "=== Looper Rust — Full Verification ==="
echo ""

echo "1. Formatting..."
cargo fmt --check
echo "✅ Formatting OK"
echo ""

echo "2. Clippy (core crates)..."
cargo clippy --lib -p looper-types -p looper-storage -p looper-api -p looper-cli -p looper-service -- -D warnings
echo "✅ Clippy OK"
echo ""

echo "3. Tests..."
cargo test --workspace
echo "✅ Tests OK"
echo ""

echo "4. Build (release)..."
cargo build --release -p looperd -p looper-cli 2>/dev/null
echo "✅ Build OK"
echo ""

echo "=== All checks passed ==="
