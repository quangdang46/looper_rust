---
name: looper-qa
description: "Use when a Looper-managed GitHub repo needs scheduled pre-merge QA — a PR carries the `looper:qa` label, the spec stage reaches `looper:spec-ready`, or the Looper reviewer loop requests an independent second pass. Runs the full QA cycle (Looper state probe → PR checkout → ffs code review → language-specific test suite → optional computer_use UI smoke → GitHub review post with structured verdict and label update). Triggers on Hermes cron ticks, on demand from `looper-qa.sh`, or whenever a Looper loop asks for an independent review."
version: 1.0.0
author: Looper + Hermes integration
license: MIT
platforms: [linux, macos]
metadata:
  hermes:
    tags: [looper, qa, github, code-review, hermes-cron, gh-cli, ffs, computer-use]
    related_skills: [requesting-code-review, plan, github-code-review]
---

# Looper QA Cycle (Hermes-driven)

Scheduled QA pass over Looper-managed pull requests. Fills the gap between
`looperd`'s own reviewer loop and the human merge step: runs an independent
review pass on every PR that carries the `looper:qa` label, posts a structured
verdict back to GitHub, and updates labels so the Looper fixer loop can pick up
regressions automatically.

**Core principle:** No single agent should be the only thing standing between
a PR and `main`. This skill is the independent reviewer.

## When to Use

- Triggered by Hermes cron every 15 minutes (`*/15 * * * *`) per Looper project
- On demand: `looper-qa.sh owner/repo#42`
- After a Looper worker loop opens a PR — pick the PR up before the human merge
- After a Looper reviewer passes — independent second-pass verification
- **Skip for:** documentation-only PRs (no Go/Rust/JS/Python code change), PRs
  already carrying `looper:qa-failed` (Fixer owns the next pass), PRs that touch
  only the `dist/` tree

## Prerequisites

Required on PATH:

| Tool | Purpose | Install check |
|------|---------|---------------|
| `looper` | Loop inspection (`ps`, `queue`, `loop failures`) | `looper version` |
| `gh` | GitHub CLI for labels, reviews, checkout | `gh auth status` |
| `git` | Worktree-less checkout via `git fetch` | `git --version` |
| `ffs` | Code review (impact, refs, flow) | `ffs --version` |
| `hermes` | Owning scheduler (this skill runs inside `hermes cron tick`) | `hermes --version` |
| `python3` | YAML lint, status parsing | `python3 --version` |
| One of: `go`, `cargo`, `npm`, `pytest` | Language test runner | auto-detected |

Optional:

- `computer_use` MCP tool (UI smoke tests for web/desktop apps)
- `looperd` running locally (only needed for the loop-status probe — graceful
  skip when unreachable)

## State Machine

```
                    cron tick (every 15 min)
                              |
                              v
                  probe looper ps --json --all
                              |
              +---------------+---------------+
              v               v               v
       no PRs eligible   PR with looper:qa   PR with looper:qa-testing
       (silent exit)     label found         (skip — in progress)
                              v
                  checkout + ffs review + tests
                              v
                  +-----------+-----------+
                  v                       v
           all checks pass        any check fails
                  v                       v
        gh pr review --approve   gh pr review --request-changes
        add looper:qa-passed     add looper:qa-failed
        (human merge)            (Fixer auto-pickup)
```

## Workflow

### Step 1 — Probe Looper state

Read the running-loop snapshot to learn which projects are active. Skip when
`looperd` is unreachable; the skill still works on PR-level state alone.

```bash
looper ps --json --all 2>/dev/null || echo "looperd unreachable, falling back to gh-only mode"
```

If the response contains `running` loops of type `reviewer` or `worker`, prefer
PRs from those projects (most likely to be the next merge candidate).

### Step 2 — Find a PR needing QA

```bash
# Primary: explicit QA label
gh pr list --label "looper:qa" \
  --json number,title,headRefName,baseRefName,url,labels,isDraft \
  --limit 1

# Fallback: spec-ready PRs not yet reviewed by Hermes
gh pr list --label "looper:spec-ready" --json ... --limit 5
```

Selection rules (in order):

1. **Skip drafts.** Draft PRs are not QA candidates.
2. **Skip `looper:qa-testing`.** Another Hermes tick is on it.
3. **Prefer oldest matching.** FIFO avoids starving slow PRs.
4. **Prefer non-failed.** PRs with `looper:qa-failed` already have a Fixer
   on them; skip this tick to give Fixer room.

If the list is empty, exit cleanly with `[SILENT]` — the cron scheduler treats
empty stdout as "no message, no notification".

### Step 3 — Checkout the PR

Use `git fetch` + worktree-less checkout so the cron tick doesn't pollute the
`~/Projects` tree:

```bash
WORKTREE_DIR="$(mktemp -d -t looper-qa-XXXXXX)"
cd "$WORKTREE_DIR"
git init -q
git remote add origin "git@github.com:${OWNER}/${REPO}.git"
git fetch --depth=1 origin "pull/${PR}/head:qa-${PR}" 2>&1 | tail -3
git checkout -q "qa-${PR}"
```

For repos where you have a full clone already (the common case when `looperd`
runs locally), check the existing worktree instead:

```bash
WORKTREE_DIR="$(looper worktree show "${OWNER}/${REPO}" --json | jq -r '.path')"
cd "$WORKTREE_DIR"
git fetch origin "pull/${PR}/head:qa-${PR}"
git checkout -q "qa-${PR}"
```

The worktree path is recorded in a `looper-qa-${PR}.env` file at `~/.hermes/state/`
so subsequent ticks (or the Fixer loop) can find the same checkout.

### Step 4 — ffs code review

See `references/review-template.md` for the full template. The minimum set:

```bash
ENTRY="$(git diff origin/${BASE}..HEAD --name-only | grep -E '\.(go|rs|py|ts)$' | head -3 | xargs -I{} ffs symbol {} | jq -r '.[0].name')"
ffs impact "$ENTRY" 2>&1 | tee /tmp/looper-qa-${PR}-impact.txt

git diff origin/${BASE}..HEAD --name-only | while read f; do
  ffs refs "$(basename "$f" | sed 's/\..*//')" 2>&1 | tee -a /tmp/looper-qa-${PR}-refs.txt
done
```

If `ffs` is missing or fails, fall back to `git grep` + `rg` (the helpers in
`looper-qa.sh` detect this automatically).

### Step 5 — Test suite

Run the language-appropriate test command. See `references/test-suites.md` for
the full per-language matrix.

```bash
make -s check 2>/dev/null \
  || make -s test 2>/dev/null \
  || go test ./... 2>&1 | tail -20 \
  || cargo test 2>&1 | tail -20 \
  || npm test --silent 2>&1 | tail -20 \
  || python -m pytest -q 2>&1 | tail -20
```

Capture pass/fail counts as `<total>/<passed>` so the report can render
`142/145 pass (3 pre-existing failures)`. Pre-existing failures are detected by
comparing against the base branch:

```bash
git checkout origin/${BASE} -- .
<run baseline tests>
git checkout -
```

If `git checkout origin/${BASE}` would touch too many files (monorepo), skip
baseline detection — it's not worth the time.

### Step 6 — UI smoke (optional, web/desktop only)

Skip this step unless the PR description or labels indicate a UI change. See
`references/ui-test-template.md` for the full template.

```bash
git diff origin/${BASE}..HEAD --name-only \
  | grep -qE '\.(tsx?|jsx?|vue|svelte|css|scss)$|/(components|pages|views|app)/'
```

If yes and `computer_use` is configured, run the standard happy-path:

1. `capture` the staging URL → screenshot to `~/.hermes/state/looper-qa-${PR}-1.png`
2. `click` the primary CTA
3. `type` a sample query
4. `capture` again → `2.png`
5. Diff the two screenshots; report failure if the layout regresses

Always timebox UI testing to 90 seconds. Move on with `[SILENT]` for the UI
section if `computer_use` times out — don't block the rest of the report.

### Step 7 — Post review to GitHub

Compose the review body and submit with a single command:

```bash
REVIEW_BODY="$(cat <<EOF
## Hermes QA Report

**PR:** #${PR}
**Verdict:** ${VERDICT}    // APPROVE | REQUEST_CHANGES | COMMENT

### Code Review (ffs)
${IMPACT_SUMMARY}

### Test Suite
- ${TEST_SUMMARY}
- baseline: ${BASELINE_SUMMARY}

### UI Smoke
${UI_SUMMARY}

### Findings
${FINDINGS_LIST}    // bullet list, format: "- [SEVERITY] path:line: message"

---
Hermes QA cycle ran at ${TIMESTAMP}. See \`~/.hermes/state/looper-qa-${PR}/\`.
EOF
)"

gh pr review "${PR}" --repo "${OWNER}/${REPO}" \
  --${VERDICT_LOWER} \
  --body "${REVIEW_BODY}"

case "${VERDICT}" in
  APPROVE)
    gh pr edit "${PR}" --repo "${OWNER}/${REPO}" --add-label "looper:qa-passed" --remove-label "looper:qa,looper:qa-failed"
    ;;
  REQUEST_CHANGES)
    gh pr edit "${PR}" --repo "${OWNER}/${REPO}" --add-label "looper:qa-failed" --remove-label "looper:qa,looper:qa-passed"
    ;;
esac
```

### Step 8 — Hand off to Fixer (failure path only)

On `REQUEST_CHANGES`, the Looper Fixer loop should auto-pickup the next tick.
Verify it does:

```bash
sleep 30
looper ps --json --all | jq '.[] | select(.type=="fixer" and .pr=="'"${OWNER}/${REPO}#${PR}"'" and .status=="running")'
```

If no Fixer is running within 60 seconds, log the failure and let the next
cron tick retry — do not start a Fixer manually from this skill.

## Output Contract

Every cron tick MUST end with one of:

| Stdout | Meaning |
|--------|---------|
| `[SILENT]` | No PR to QA this tick |
| `QA PASSED: <owner>/<repo>#<PR>` | All checks clean |
| `QA FAILED: <owner>/<repo>#<PR>` | One or more checks failed; review posted |
| `QA ERROR: <owner>/<repo>#<PR>: <message>` | Tool failure, will retry next tick |

The cron scheduler delivers stdout verbatim when non-empty; stay terse. Full
report lives in `~/.hermes/state/looper-qa-${PR}/report.md` and on the PR
itself.

## Common Pitfalls

1. **Reviewing a draft PR.** Always filter `--json isDraft --jq '.[] | select(.isDraft==false)'`.
2. **`looper ps --json` schema drift.** The schema is `[{seq,type,pr,status}]` in
   current versions. If `looper version` reports a different shape, fall back
   to `looper ps` (text mode) and grep for `running`.
3. **Testing without a clean baseline.** `git status` should be clean before
   running tests; if not, the test command above will pick up unrelated diff.
4. **Cross-repo contamination.** Always `cd` into the per-tick worktree; never
   `cd $HOME/Projects/foo` blindly.
5. **`gh pr review --request-changes` requires comment body.** Some shell
   configs strip empty bodies — always pass `--body`.
6. **`computer_use` user conflict.** UI smoke tests steal the user's cursor on
   macOS. Run during low-activity hours or set `LOOPER_QA_UI_SKIP=1` to skip.
7. **Rate limit on `gh`.** `gh api rate-limit` shows remaining; if under 100,
   skip the tick with `[SILENT]`.
8. **Mismatched `BASE`.** Resolve with `git symbolic-ref refs/remotes/origin/HEAD`
   or pass `--base` to `gh pr view`; never assume `main`.

## Verification Checklist

Before declaring the cron job installed and the skill active:

- [ ] `~/.hermes/skills/looper-qa/SKILL.md` exists with valid frontmatter
- [ ] `hermes cron list` shows the `looper-qa` job
- [ ] `hermes cron run <job_id>` produces a non-error exit and stdout matching
      the Output Contract above
- [ ] At least one PR has been QA'd end-to-end with a posted review
- [ ] Labels `looper:qa`, `looper:qa-passed`, `looper:qa-failed`,
      `looper:qa-testing` exist on the target repo (run `looper labels init`
      or `gh label create` for missing ones)
- [ ] Looper Fixer loop picks up `looper:qa-failed` PRs on the next tick
- [ ] `~/.hermes/state/looper-qa-*/` keeps last 30 days of reports (prune
      older entries)

## Related Skills

- `requesting-code-review` — pre-commit verification on the local working tree
- `plan` — Looper's spec drafting loop (this skill reviews the *output* of plan)
- `github-code-review` — interactive GitHub PR review (this skill runs the same
  checks but unattended, on a cron schedule)
