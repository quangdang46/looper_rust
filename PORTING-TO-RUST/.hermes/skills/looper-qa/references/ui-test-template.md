# Computer Use UI Smoke Template

Lightweight UI regression check using Hermes's `computer_use` MCP tool. Used
in Step 6 of the main `SKILL.md` workflow.

## When to run

Run only when the PR diff includes UI files:

```bash
git diff origin/${BASE}..HEAD --name-only \
  | grep -qE '\.(tsx?|jsx?|vue|svelte|css|scss|html)$|/(components|pages|views|app|screens)/'
```

If no UI files, skip silently with `[SILENT]`.

## 90-second happy path

Always timebox to 90 seconds. If `computer_use` is slower or fails, skip the
UI section — don't block the rest of the report.

### Step 1 — Capture initial state

```python
computer_use(
    action="capture",
    mode="screenshot",
    app="Safari",                       # or "Chrome", or omit for active app
    output_path=f"~/.hermes/state/looper-qa-{PR}/ui-1.png",
)
```

### Step 2 — Detect URL

```bash
URL="$(gh pr view ${PR} --json body -q .body \
  | grep -oE 'https?://[^ )]+' \
  | grep -E 'localhost|staging|preview' \
  | head -1)"
[ -z "$URL" ] && URL="http://localhost:3000"   # fallback for local-only apps
```

### Step 3 — Primary CTA test

```python
computer_use(action="navigate", url=f"{URL}/dashboard")
computer_use(action="capture", mode="screenshot", output_path=".../ui-2.png")
# Click the most prominent CTA (heuristic: top-right button)
computer_use(action="click", element="primary-cta", confidence=0.7)
computer_use(action="capture", mode="screenshot", output_path=".../ui-3.png")
```

### Step 4 — Form input test (only if a form is present)

```python
computer_use(action="type", text="Hermes QA test query", field="search-input")
computer_use(action="press_key", key="Return")
computer_use(action="capture", mode="screenshot", output_path=".../ui-4.png")
```

### Step 5 — Diff against baseline

```bash
# Compare to a stored baseline (if the repo has one)
if [ -f ~/.hermes/state/ui-baseline.png ]; then
  python3 - <<'PY'
from PIL import Image, ImageChops
a = Image.open(os.path.expanduser("~/.hermes/state/ui-baseline.png"))
b = Image.open(".../ui-4.png")
diff = ImageChops.difference(a, b)
bbox = diff.getbbox()
print("DIFF_BBOX:", bbox or "identical")
PY
fi
```

If `DIFF_BBOX` is non-null and exceeds 10% of the viewport, flag it as a
regression in the report.

## Failure handling

| Symptom | Action |
|---------|--------|
| `computer_use` not available | skip with `[SILENT]` |
| `computer_use` raises on first call | record error, proceed without UI section |
| Screenshot is identical to baseline | mark UI section as PASS |
| Screenshot diff exceeds 10% | mark UI section as WARN, add to findings |

## Opting out

Set `LOOPER_QA_UI_SKIP=1` in the environment to disable UI testing entirely
(useful when running on a headless server).

## Sample QA report snippet

```
### UI Smoke
- ✅ Dashboard renders without errors
- ✅ Primary CTA reachable
- ⚠️  Search input field: layout shifted 12% from baseline
  - baseline: `~/.hermes/state/ui-baseline.png`
  - current:  `~/.hermes/state/looper-qa-${PR}/ui-4.png`
```
