# ffs Code Review Template

Concrete `ffs` invocation patterns for the `looper-qa` skill. Used in
Step 4 of the main `SKILL.md` workflow.

## Quick reference

| Goal | Command |
|------|---------|
| Find the main entry point of a change | `ffs symbol <file> \| jq -r '.[0].name'` |
| List callers of a changed function | `ffs callers <symbol>` |
| List callees of a changed function | `ffs callees <symbol>` |
| Find all definitions of a symbol | `ffs refs <symbol>` |
| Drill into one symbol's call envelope | `ffs flow <symbol>` |
| Rank files affected by a change | `ffs impact <symbol>` |
| Search file contents across the repo | `ffs multi_grep "pattern1\|pattern2"` |
| Structural outline of one file | `ffs outline <file>` |

## Standard QA pass

Run from inside the per-tick worktree:

```bash
PR="${PR:-42}"
BASE="${BASE:-main}"

# 1. List of changed files (Go convention; adjust for other languages)
CHANGED="$(git diff origin/${BASE}..HEAD --name-only --diff-filter=ACMRT)"

# 2. For each non-test file, get the top-level symbol and inspect impact
for f in $CHANGED; do
  case "$f" in
    *_test.go) continue ;;
    *.go)
      main_sym="$(ffs symbol "$f" 2>/dev/null | jq -r '.[0].name // empty')"
      if [ -n "$main_sym" ]; then
        echo "=== Impact of $main_sym ($f) ==="
        ffs impact "$main_sym" 2>&1 | head -20
        echo
      fi
      ;;
    *.rs)
      ffs outline "$f" 2>&1 | head -30
      ;;
    *.py|*.ts)
      ffs multi_grep "^$(basename "$f" .py)" 2>&1 | head -15
      ;;
  esac
done | tee "/tmp/looper-qa-${PR}-impact.txt"
```

## Failure-mode heuristics

When `ffs` is missing or returns an error:

```bash
which ffs || {
  echo "ffs not installed; using rg fallback"
  rg --pcre2 -n 'TODO|FIXME|XXX|HACK' . 2>&1 | head -10
}
```

If `ffs` exists but errors on a specific symbol (rare):

```bash
# Degrade gracefully: log and continue
ffs impact "$main_sym" 2>&1 | tee -a "/tmp/looper-qa-${PR}-impact.txt" \
  || echo "ffs impact failed for $main_sym — skipping" >> "/tmp/looper-qa-${PR}-impact.txt"
```

## Output schema

The review output should reduce to these fields for the GitHub review body:

- `impacted_files` — integer from `ffs impact | wc -l`
- `callers_count` — integer from `ffs callers | jq 'length'`
- `new_symbols` — list of `(file, line, name)` tuples from `git diff | grep '^+func'`
- `delete_symbols` — list of `(file, line, name)` tuples from `git diff | grep '^-func'`

Skip the report section if all four fields are zero — that's a docs-only
change and should fall through to the test suite check.

## Truncation safety

`ffs impact` can return thousands of lines on a busy symbol. Truncate to
first 50 lines for the GitHub body:

```bash
{
  echo "### Impact (top 50 lines)"
  head -50 "/tmp/looper-qa-${PR}-impact.txt"
  echo
  echo "_Full output: ~/.hermes/state/looper-qa-${PR}/impact.txt_"
} > /tmp/looper-qa-${PR}-impact.snippet.txt
```
