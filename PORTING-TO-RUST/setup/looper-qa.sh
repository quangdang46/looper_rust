#!/usr/bin/env bash
# looper-qa.sh — manual trigger + apply-cron helper for the looper-qa skill.
#
# Usage:
#   looper-qa.sh                       # run one QA tick for $LOOPER_QA_REPOS
#   looper-qa.sh owner/repo#42         # QA one explicit PR
#   looper-qa.sh --apply-cron          # register the cron job (idempotent)
#   looper-qa.sh --remove              # unregister the cron job
#   looper-qa.sh --status              # show cron + last QA reports + labels
#   looper-qa.sh --remove-state        # clear old reports under ~/.hermes/state

set -euo pipefail

LOOPER_QA_HOME="${LOOPER_QA_HOME:-$HOME/.hermes}"
SKILL_DIR="$LOOPER_QA_HOME/skills/looper-qa"
CRON_FILE="$LOOPER_QA_HOME/cron/looper-qa.yaml"
STATE_DIR="$LOOPER_QA_HOME/state"

REPOS_DEFAULT="${REPOS_DEFAULT:-nexu-io/looper}"
REPOS="${LOOPER_QA_REPOS:-$REPOS_DEFAULT}"

usage() {
  sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

require_tools() {
  for t in gh git python3; do
    command -v "$t" >/dev/null 2>&1 || { echo "missing required tool: $t" >&2; exit 1; }
  done
  command -v hermes >/dev/null 2>&1 || { echo "missing: hermes (needed for --apply-cron)" >&2; exit 1; }
}

ensure_state() { mkdir -p "$STATE_DIR"; }

apply_cron() {
  require_tools
  [ -f "$CRON_FILE" ] || { echo "missing: $CRON_FILE" >&2; exit 1; }

  local schedule name prompt
  schedule=$(awk '/^schedule:/{gsub(/[" ]/,"",$2); print $2}' "$CRON_FILE")
  name=$(awk '/^name:/{print $2; exit}' "$CRON_FILE")

  if hermes cron list 2>/dev/null | grep -qE "[[:space:]]${name}[[:space:]]|\"${name}\""; then
    echo "Cron job '$name' already registered; skipping create."
    hermes cron list | grep -E "$name" || true
    return 0
  fi

  prompt="$(awk '/^prompt:/{flag=1; next} /^[^[:space:]#]/ && flag {flag=0} flag' "$CRON_FILE")"

  hermes cron create "$schedule" \
    --name "$name" \
    --skill looper-qa \
    --workdir "$HOME/Projects" \
    --deliver local \
    "$prompt"
}

remove_cron() {
  require_tools
  if hermes cron list 2>/dev/null | grep -q looper-qa; then
    hermes cron remove --name looper-qa 2>/dev/null \
      || hermes cron rm looper-qa 2>/dev/null \
      || true
    echo "Cron job looper-qa removed."
  else
    echo "No looper-qa cron job registered."
  fi
}

status() {
  echo "=== hermes cron ==="
  hermes cron list 2>/dev/null | grep -E "looper-qa" || echo "(no looper-qa job)"
  echo
  echo "=== last 5 reports ==="
  ls -1t "$STATE_DIR"/looper-qa-*/report.md 2>/dev/null | head -5 \
    || echo "(no reports yet)"
  echo
  echo "=== labels on ${REPOS_DEFAULT} ==="
  gh label list --repo "$REPOS_DEFAULT" 2>/dev/null \
    | grep -E "looper:qa" \
    || echo "(no looper:qa labels — run: gh label create looper:qa-passed --color 2CBE4E ...)"
}

run_one_pr() {
  local target="$1"   # owner/repo#PR or just a PR number
  require_tools
  ensure_state

  # Resolve target
  if [[ "$target" =~ ^[0-9]+$ ]]; then
    target="${REPOS_DEFAULT}#${target}"
  fi
  local owner_repo="${target%#*}"
  local pr="${target##*#}"

  export LOOPER_QA_REPOS="$owner_repo"
  local state_dir="$STATE_DIR/looper-qa-${pr}"
  mkdir -p "$state_dir"

  echo "→ Probing $target"
  if gh pr view "$pr" --repo "$owner_repo" --json isDraft -q .isDraft 2>/dev/null | grep -q true; then
    echo "QA SKIPPED: draft PR"
    return 0
  fi

  # Checkout (git fetch to a temp worktree to avoid touching ~/Projects)
  local workdir
  workdir="$(mktemp -d -t looper-qa-XXXXXX)"
  cd "$workdir"
  git init -q
  git remote add origin "git@github.com:${owner_repo}.git"
  git fetch --depth=1 origin "pull/${pr}/head:qa-${pr}" >/dev/null 2>&1 || {
    echo "QA ERROR: $target — fetch failed" >&2
    return 1
  }
  git checkout -q "qa-${pr}"

  # Auto-detect test runner
  local runner=""
  [ -f Makefile ] && grep -qE '^check:|^test:' Makefile && runner="make"
  [ -z "$runner" ] && [ -f go.mod ] && runner="go"
  [ -z "$runner" ] && [ -f Cargo.toml ] && runner="rust"
  [ -z "$runner" ] && [ -f package.json ] && grep -qE '"test"\s*:' package.json && runner="node"
  [ -z "$runner" ] && { [ -f pyproject.toml ] || [ -f setup.py ] || [ -f pytest.ini ]; } && runner="python"

  echo "→ Detected runner: ${runner:-<none>}"

  # Run tests with a per-runner timeout
  local timeout=180
  [ "$runner" = "rust" ] && timeout=600

  case "$runner" in
    make)   timeout "${timeout}s" make -s check 2>&1 | tee "$state_dir/test.log" | tail -30 ;;
    go)     timeout "${timeout}s" go test ./...  2>&1 | tee "$state_dir/test.log" | tail -30 ;;
    rust)   timeout "${timeout}s" cargo test     2>&1 | tee "$state_dir/test.log" | tail -30 ;;
    node)   timeout "${timeout}s" npm test --silent 2>&1 | tee "$state_dir/test.log" | tail -30 ;;
    python) timeout "${timeout}s" python -m pytest -q --tb=no 2>&1 | tee "$state_dir/test.log" | tail -30 ;;
    *)
      echo "(no test runner detected — skipping test step)" > "$state_dir/test.log"
      ;;
  esac || true

  # Parse verdict from the log
  local passes fails verdict label body
  passes=$(grep -cE '^(ok |.*passing|.*PASS)' "$state_dir/test.log" 2>/dev/null || echo 0)
  fails=$(grep -cE '^(FAIL|.*failing|.*FAILED)' "$state_dir/test.log" 2>/dev/null || echo 0)

  if [ "$fails" -eq 0 ]; then
    verdict="APPROVE"; label="looper:qa-passed"
  else
    verdict="REQUEST_CHANGES"; label="looper:qa-failed"
  fi

  body=$(printf "## Hermes QA Report\n\n**Verdict:** %s\n**Test:** %s pass / %s fail\n\n_Ran at %s from looper-qa.sh._\n" \
    "$verdict" "$passes" "$fails" "$(date -u +%FT%TZ)")

  gh pr review "$pr" --repo "$owner_repo" \
    --$(echo "$verdict" | tr 'A-Z' 'a-z' | tr '_' '-') \
    --body "$body" \
    2>&1 | tee -a "$state_dir/test.log"

  gh pr edit "$pr" --repo "$owner_repo" \
    --add-label "$label" \
    --remove-label "looper:qa,looper:qa-failed,looper:qa-passed" \
    2>&1 | tee -a "$state_dir/test.log"

  cp "$state_dir/test.log" "$state_dir/report.md"

  if [ "$verdict" = "APPROVE" ]; then
    echo "QA PASSED: $target"
  else
    echo "QA FAILED: $target"
  fi
}

run_tick() {
  require_tools
  ensure_state
  export LOOPER_QA_REPOS="$REPOS"

  if [ -d "$SKILL_DIR" ]; then
    echo "Reading skill from $SKILL_DIR"
  else
    echo "Skill not found at $SKILL_DIR — install it first." >&2
    exit 1
  fi

  # When running as a cron tick, the agent prompt is what does the work.
  # We just print the skill prompt to stdout so the agent has full context.
  cat "$SKILL_DIR/SKILL.md"
}

# --- main ---
case "${1:-}" in
  -h|--help|help)    usage 0 ;;
  --apply-cron)      apply_cron ;;
  --remove)          remove_cron ;;
  --status)          status ;;
  --remove-state)
    rm -rf "$STATE_DIR"/looper-qa-* 2>/dev/null || true
    echo "state cleared"
    ;;
  "")
    run_tick
    ;;
  *)
    run_one_pr "$1"
    ;;
esac
