#!/usr/bin/env bash
# Run all E2E tests against real binaries.
#
# Builds all required binaries, sets environment variables, and runs
# the looper-e2e integration tests.
#
# Usage: ./scripts/run-e2e.sh [--build-only] [--skip-build] [test_name]

set -eu -o pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

BUILD=true
TEST_FILTER=""

for arg in "$@"; do
    case "$arg" in
        --skip-build) BUILD=false ;;
        --build-only) BUILD=true; TEST_FILTER="__none__" ;;
        *) TEST_FILTER="$arg" ;;
    esac
done

if $BUILD; then
    echo "==> Building binaries..."
    cargo build --release -p looperd -p looper-cli -p looper-e2e 2>&1
    echo "==> Build complete."
fi

if [ "${TEST_FILTER:-}" = "__none__" ]; then
    echo "==> Build-only mode. Exiting."
    exit 0
fi

echo "==> Setting E2E environment variables..."
export LOOPER_E2E_LOOPER_PATH="$REPO_ROOT/target/release/looper-cli"
export LOOPER_E2E_LOOPERD_PATH="$REPO_ROOT/target/release/looperd"
export LOOPER_E2E_FAKE_AGENT_PATH="$REPO_ROOT/target/release/fake-agent"
export LOOPER_E2E_FAKE_GH_PATH="$REPO_ROOT/target/release/fake-gh"
export LOOPER_E2E_FAKE_OSASCRIPT_PATH="$REPO_ROOT/target/release/fake-osascript"

echo "   LOOPER_E2E_LOOPER_PATH=$LOOPER_E2E_LOOPER_PATH"
echo "   LOOPER_E2E_LOOPERD_PATH=$LOOPER_E2E_LOOPERD_PATH"
echo "   LOOPER_E2E_FAKE_AGENT_PATH=$LOOPER_E2E_FAKE_AGENT_PATH"
echo "   LOOPER_E2E_FAKE_GH_PATH=$LOOPER_E2E_FAKE_GH_PATH"
echo "   LOOPER_E2E_FAKE_OSASCRIPT_PATH=$LOOPER_E2E_FAKE_OSASCRIPT_PATH"

echo "==> Running E2E tests..."
if [ -n "${TEST_FILTER:-}" ]; then
    exec cargo test -p looper-e2e --test smoke_daemon_test -- "$TEST_FILTER"
else
    exec cargo test -p looper-e2e --test smoke_daemon_test
fi
