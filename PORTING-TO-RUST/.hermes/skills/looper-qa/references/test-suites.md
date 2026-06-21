# Test Suite Reference

Per-language detection and invocation patterns for the `looper-qa` skill.
Used in Step 5 of the main `SKILL.md` workflow.

## Detection priority

Order matters — the first match wins.

```bash
detect_test_runner() {
  cd "$WORKTREE_DIR" || return 1

  # Monorepo / multi-language
  if [ -f "Makefile" ] && grep -qE '^check:|^test:' Makefile; then
    echo "make"; return 0
  fi

  # Go (looper's own language)
  if [ -f "go.mod" ]; then
    echo "go"; return 0
  fi

  # Rust
  if [ -f "Cargo.toml" ]; then
    echo "rust"; return 0
  fi

  # Node / TypeScript
  if [ -f "package.json" ] && grep -qE '"test"\s*:' package.json; then
    echo "node"; return 0
  fi

  # Python
  if [ -f "pyproject.toml" ] || [ -f "setup.py" ] || [ -f "pytest.ini" ]; then
    echo "python"; return 0
  fi

  # Last resort
  echo "unknown"
}
```

## Per-language commands

### Make (most CI-driven projects)

```bash
make check 2>&1 | tail -30    # runs lint + vet + test
make test  2>&1 | tail -30    # tests only
```

### Go (Looper, hermes-agent)

```bash
go test ./... 2>&1 | tail -30
go vet ./...   2>&1 | tail -10   # always run after test
```

The Go test output line `ok  pkg  0.123s` or `FAIL pkg  0.123s` is the source
for pass/fail counts. Parse:

```bash
GO_RESULT="$(go test ./... 2>&1)"
PASSES=$(echo "$GO_RESULT" | grep -c '^ok ')
FAILS=$(echo "$GO_RESULT" | grep -c '^FAIL')
echo "$PASSES/$((PASSES + FAILS)) pass"
```

### Rust

```bash
cargo test 2>&1 | tail -30
cargo clippy -- -D warnings 2>&1 | tail -10   # only if clippy is in tree
```

### Node / TypeScript

```bash
npm test --silent 2>&1 | tail -30
# or, for TypeScript projects:
npx tsc --noEmit 2>&1 | tail -10
```

### Python

```bash
python -m pytest -q --tb=no 2>&1 | tail -30
which ruff && ruff check . 2>&1 | tail -10    # only if installed
```

## Pass/fail parsing

For every runner, normalize the output to PASS/FAIL lines and count:

```bash
normalize() {
  local runner="$1" output="$2"
  case "$runner" in
    make|go)
      echo "$output" | grep -cE '^(ok|FAIL)'
      ;;
    rust)
      echo "$output" | grep -cE '^test result:'
      ;;
    node)
      echo "$output" | grep -cE '(passing|failing|✓|✗)'
      ;;
    python)
      echo "$output" | grep -oE '[0-9]+ passed' | head -1
      ;;
  esac
}
```

## Baseline comparison

Detect pre-existing failures by running the same suite on the base branch:

```bash
git stash -u
git checkout origin/${BASE}
<run baseline>
BASELINE_RESULT=$?
git checkout -
git stash pop
```

If baseline already had failures, subtract them from the new run:

```bash
NEW_FAILS=$((CURRENT_FAILS - BASELINE_FAILS))
[ "$NEW_FAILS" -eq 0 ] && echo "no regressions"
```

If `NEW_FAILS > 0`, escalate to REQUEST_CHANGES with the diff of failing tests
between base and HEAD in the report body.

## Time-boxing

Always set a per-runner timeout. Test suites that hang block the cron tick:

```bash
case "$runner" in
  make|go|node|python) timeout=180 ;;
  rust)                timeout=600 ;;   # cargo cold-cache can be slow
esac

timeout "${timeout}s" <runner-command> 2>&1 | tail -30
TIMEOUT_RC=$?
[ "$TIMEOUT_RC" -eq 124 ] && echo "TIMEOUT after ${timeout}s — flag as flake"
```

## Verdict mapping

| Result | Verdict | Label |
|--------|---------|-------|
| All pass, no baseline | `APPROVE` | `looper:qa-passed` |
| All pass, baseline had same failures | `APPROVE` | `looper:qa-passed` |
| New failures vs baseline | `REQUEST_CHANGES` | `looper:qa-failed` |
| Test runner not found | `COMMENT` | (no label change) |
| Timeout | `COMMENT` (flag as flake) | (no label change) |

Use `COMMENT` for cases where the QA cycle can't make a confident call —
don't punish the PR for infrastructure issues.
