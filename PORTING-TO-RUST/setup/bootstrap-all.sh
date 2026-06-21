#!/bin/bash
# Bootstrap all repos into Looper + create test issues
set -e

# Repos to skip (looper itself, hermes-agent which is not owned)
SKIP="looper hermes-agent"

echo "=== Bootstrapping Looper projects ==="
for d in ~/Projects/*/; do
  name=$(basename "$d")
  [[ $SKIP =~ (^|[[:space:]])$name($|[[:space:]]) ]] && echo "  SKIP $name" && continue
  [ ! -d "$d/.git" ] && continue

  echo ""
  echo "--- $name ---"

  # Add project (idempotent -- will update if exists)
  looper project add "$d" --name "$name" 2>&1 | head -3

  # Init labels
  cd "$d"
  looper labels init 2>&1 | tail -3

  # Create test issue
  gh issue create \
    --title "TEST: Looper integration test for $name" \
    --body "## Test issue for Looper integration

This is an automated test to verify Looper can:
- Pick up issues labeled \`looper:plan\`
- Run planner/reviewer/fixer/worker loops
- Create spec PRs
- Implement fixes

**Expected:** Looper creates a spec PR, reviews it, and marks it ready.

**To trigger:** Add label \`looper:plan\` and assign to quangdang46
" \
    --label "bug" \
    2>&1 | head -1
done

echo ""
echo "=== Done! ==="
echo "Next: Add 'looper:plan' label + assign yourself to any test issue to trigger Looper"
