# grove

**Write code while you sleep. Complete all your beads tasks with one command.**

---

## The Pain That Built This

You know the workflow.

Open terminal. Run `claude`. Paste the init prompt — the one that loads context, explains the project, tells the agent what it's working on. Wait for it to understand. Finally, the agent starts working.

Then the context limit hits.

You exit. You open a new session. You paste the init prompt again. Load context again. Wait again. Resume where it left off — manually, because nothing remembers.

You can't leave. You can't do other work. You sit there, watching, waiting for the next context limit so you can loop it again. The agent does the work. But you're the one who can't walk away.

```
open claude
paste init prompt
load context
<agent works>
context limit hit
exit
open claude again
paste init prompt again
...
...repeat until all beads done
...or until you give up for the night
```

You become the orchestrator. A human one. Manually chaining sessions, one at a time, unable to stop because the moment you step away the work stops too.

**Grove closes this loop.**

Define your tasks with `br`. Type `grove run`. Walk away. Come back to completed work — sessions handled, context rotations managed, internal workflow phases advanced automatically, and native handoffs, transcript archive, and playbook memory carried forward automatically. Anything that failed or couldn't mirror back to `br` is flagged, not silently lost.

---

## Standing on Shoulders

Grove didn't appear from nothing. The exit gate and circuit breaker come from [Frank Bria](https://github.com/frankbria)'s [ralph-claude-code](https://github.com/frankbria/ralph-claude-code), which proved that an autonomous Claude loop needs both heuristic detection and an explicit exit signal to avoid premature stops. The entire task graph — dependencies, lifecycle, ready-queue — runs on [Jeff Emanuel](https://github.com/Dicklesworthstone)'s [beads_rust](https://github.com/Dicklesworthstone/beads_rust), and grove uses his [beads_viewer](https://github.com/Dicklesworthstone/beads_viewer) for PageRank, critical path, and triage scoring when deciding which bead to dispatch next. Grove's native transcript archive is modeled after his [coding_agent_session_search](https://github.com/Dicklesworthstone/coding_agent_session_search), and the playbook engine — evidence scoring, confidence decay, curation, anti-pattern inversion — is adapted directly from his [cass_memory_system](https://github.com/Dicklesworthstone/cass_memory_system). His [ntm](https://github.com/Dicklesworthstone/ntm) shaped early thinking about session layout and parallel coordination, though grove chose direct process spawning over tmux. Finally, [nwiizo](https://github.com/nwiizo)'s [ccswarm](https://github.com/nwiizo/ccswarm) influenced the Rust workspace structure, type-state patterns, and task scoring design. Thank you all.

---

## Workflow

![Grove workflow overview](assets/workflow.png)

*End-to-end Grove workflow from bead creation to mirrored completion.*

---

## How It Works

Grove runs a continuous autonomous loop over your beads task graph. Each bead is dispatched to the configured provider runtime session. The coordinator can keep multiple sessions in flight concurrently up to `max_parallel`, while still enforcing file reservation safety and a single active leader lease. When context exhausts, grove checkpoints and spawns a fresh session automatically. Child beads inherit structured handoffs from parents.

Ordinary `task` beads still run directly. Workflow beads, currently `feature` and `epic`, are handled internally as a multi-phase chain:

`explore -> plan -> validate -> execute -> review -> compound`

That phase chain is not a new CLI. It is internal behavior inside `grove run`. Intermediate workflow phases do not close the bead in `br`. Only terminal success after the final phase mirrors and closes the parent bead.

### The Loop

```
grove run
  │
  ├─ sync br ready --json
  │     → [bd-e9b1d4, bd-7f3a2c]  (no blockers, both ready)
  │
  ├─ score candidates (priority + critical path + bv triage insights)
  │
  ├─ dispatch top-scoring beads (up to max_parallel)
  │     session A: claude -p "<task + parent handoffs + archive snippets + playbook rules>"
  │     session B: claude -p "<task + parent handoffs + archive snippets + playbook rules>"
  │
  ├─ session A outputs GROVE_EXIT: true (+ completion indicators met)
  │     → persist handoff
  │     → if bead is a workflow feature/epic, advance phase instead of closing early
  │     → plan phase may create child execution beads in `br`
  │     → index transcript into grove's native archive
  │     → extract lessons into playbook
  │     → only terminal workflow success mirrors to br (`br comment add` + `br close`)
  │     → child bead C (depends on A) becomes ready
  │     → next tick: grove dispatches C
  │
  ├─ session B hits context pressure
  │     → GROVE_CHECKPOINT: {"progress": "60% done", "next": "finish auth"}
  │     → grove persists checkpoint, ends session
  │     → spawns fresh session B' with checkpoint + full context injected
  │
  └─ loop until all beads done or shutdown
```

### Intelligent Exit Detection

Grove does not exit just because Claude says it's done. It uses a **dual-condition check**:

**Exit requires BOTH:**

1. `completion_indicators >= 2` — heuristic from natural language patterns in output
2. Claude's explicit `GROVE_EXIT: true` in the protocol block

```
Loop 5: "Phase complete, moving to next feature"
  → completion_indicators: 3
  → GROVE_EXIT: false (Claude says more work needed)
  → Result: CONTINUE

Loop 8: "All tasks complete"
  → completion_indicators: 4
  → GROVE_EXIT: true
  → Result: SUCCESS → persist handoff, unblock children
```

This prevents premature exits during productive iterations.

### Internal Workflow Beads

If a bead is a `feature` or `epic`, Grove treats it as workflow-managed work:

1. `explore` clarifies scope and constraints.
2. `plan` produces execution-ready decomposition.
3. `validate` stress-tests the plan before coding.
4. `execute` performs the actual implementation work.
5. `review` audits the result and fixes obvious defects.
6. `compound` captures durable lessons and final handoff notes.

The important behavior is in `plan`: Grove can convert planned slices into real child `task` beads in `br`, add dependencies from the parent workflow bead to those children, then keep running until those children complete and the parent can resume. The user still only runs `grove run`.

### Circuit Breaker

Grove monitors each session for stuck loops:

```
No progress for 3 iterations → circuit OPEN
Same error repeated 5 times  → circuit OPEN
Permission denied 2 times    → circuit OPEN, fail fast
```

Auto-recovery: OPEN → cooldown (30min) → HALF_OPEN → test one iteration → CLOSED.

### Context Exhaustion

When context fills up, grove spawns a **brand new session** with full memory reconstructed:

```
session running...
  estimated tokens > 82%?
    → session outputs GROVE_CHECKPOINT: {progress, next_step, context}
    → grove persists checkpoint to DB + file
    → session ends gracefully
    → new session spawned with checkpoint + parent handoffs + archive snippets + playbook rules
    → work resumes mid-task in fresh context window

  estimated tokens > 90% with no checkpoint?
    → grove synthesizes emergency checkpoint from latest protocol state
    → kills session
    → new session spawned immediately
```

### Native Memory Engine

Grove owns its memory entirely. No external memory or search tool is required.

```
Bead A session ends
  → grove indexes the transcript into its native FTS5 archive with transcript-backed provenance
  → grove persists a structured handoff (summary, artifacts, lessons, decisions, warnings)
  → grove extracts `GROVE_LESSONS` into playbook draft bullets

Bead B (child of A) dispatched
  → archive search: "auth middleware" → returns relevant snippets from past sessions
  → playbook selector: returns proven rules matching this task's scope/tags
  → parent handoff injected into prompt
  → Bead B starts knowing exactly what A did
```

Over time, repeated lessons get promoted (Candidate → Established → Proven). Harmful rules get demoted or inverted into anti-patterns. The playbook stays compact and self-curating via exponential decay scoring.

Grove also records reaction evaluations on failure paths and can persist recovery capsules plus retry-oriented guidance, so the recovery loop is no longer purely static policy metadata.

---

## Install

```bash
curl -fsSL "https://raw.githubusercontent.com/quangdang46/grove/main/install.sh?$(date +%s)" | bash
```

**Required tools (install first):**

- Provider CLI: `claude` or `codex` (install the one you plan to run Grove with)
- `br` (beads_rust) — `cargo install --git https://github.com/Dicklesworthstone/beads_rust`
- `bv` (beads_viewer) — `cargo install --git https://github.com/Dicklesworthstone/beads_viewer`

No other orchestration, memory, or search tool is required. Grove implements all memory and retrieval natively.

---

## Quick Start

```bash
cd my-project
br init

# Create tasks
br create "Set up database schema" --type task
# → bd-e9b1d4

br create "Implement auth middleware" --type task
# → bd-7f3a2c

# Workflow beads also work. Grove will handle their internal phases automatically.
br create "Ship auth system" --type feature

br dep add bd-7f3a2c bd-e9b1d4   # auth depends on schema

# Init grove with the default Claude runtime
# Add --skills to scaffold all bundled skills into .agents/skills/
grove init --skills

# Or initialize directly for Codex/OpenAI
grove init --provider codex --skills

# Optional: customize the startup prompt template grove injects into new sessions
$EDITOR .grove/startup_prompt.md

# Run — then go do something else
grove run
```

`grove init` creates a user-owned startup prompt template at `.grove/startup_prompt.md` if it does not already exist. Edit that file to change the baseline instructions Grove injects into every freshly spawned session. Re-running `grove init` will preserve your edited file unless you delete it yourself. If Grove is already initialized, use `grove sync` to refresh the bead cache instead of re-running `grove init`.

Use `grove migrate --provider codex` or `grove migrate --provider claude` to switch an existing workspace between providers without resetting unrelated Grove settings.

If you pass `grove init --skills`, Grove scaffolds all bundled skills into `.agents/skills/<skill-name>/SKILL.md`. Each scaffold is create-if-missing and becomes user-owned immediately, so reruns (including `--force`) preserve any edits you make there.

### Trick to fast to complete the plan

If you want Grove to implement all tasks faster, follow the setup below.

1. Enable Claude Code agent teams in your Claude settings:

   ```json
   {
     "env": {
       "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS": 1
     }
   }
   ```

   Agent teams docs: https://code.claude.com/docs/en/agent-teams

2. Initialize Grove with bundled swarm skills:

   ```bash
   grove init --skills
   ```

3. Change the Grove startup prompt template so new Claude sessions start with:

   ```text
   /flywheel-swarm
   ```

   Edit `.grove/startup_prompt.md` for this.

4. Start Grove:

   ```bash
   grove run
   ```

This setup makes Grove implement all tasks faster because each new Claude session starts in the swarm workflow immediately. The trade-off is higher token usage. For Codex workspaces, use the corresponding `$skill` form in the startup prompt instead of Claude's slash command form.

Always make sure `am` (MCP Agent Mail) is running before using this workflow.

After initialization, use `grove sync` to reconcile the local Grove bead cache with the current open bead set from `br` without resetting Grove-managed runtime state.

Grove handles everything from here. When it finishes, completed beads are mirrored back to `br`. For workflow beads, that mirror/close only happens after terminal success at the end of the internal phase chain. If a mirror fails, grove preserves the local result and flags it for retry — run `grove status` to see what landed and what needs attention.

---

## Usage

```bash
# Init grove workspace
grove init

# Init grove workspace for Codex/OpenAI
grove init --provider codex

# Init grove and scaffold all bundled skills for Claude Code
# into .agents/skills/<skill-name>/SKILL.md
grove init --skills

# Migrate an existing Grove workspace between providers
grove migrate --provider codex
grove migrate --provider claude

# Refresh Grove's local bead cache from br without resetting local runtime state
grove sync

# Start orchestrator (the main command)
grove run

# Start orchestrator with the live terminal UI
grove run --live

# Check status — leader lease, ready queue, running beads, checkpoints, failures, mirror-pending state
grove status

# Deep inspect a bead — dispatch reasoning, reservation conflicts, prompt manifest, retrieval snippets, playbook bullets, checkpoints, recovery capsules, handoffs, mirror actions
grove inspect bd-e9b1d4

# Show the latest run log, event log, transcript tail, and latest checkpoint or recovery capsule
grove log bd-e9b1d4

# Reset a failed or checkpointed bead so the next `grove run` can retry it
grove retry bd-e9b1d4
```

---

## Node Protocol

Grove communicates with provider runtime sessions through stdout markers:

**Task complete:**

```
GROVE_RESULT: Implemented JWT auth middleware with refresh token support
GROVE_ARTIFACTS: ["src/middleware/auth.rs", "tests/auth_test.rs"]
GROVE_LESSONS: ["Always validate token expiry before checking signature"]
GROVE_DECISIONS: ["Used RS256 for token signing"]
GROVE_WARNINGS: ["Rate limiting not yet implemented"]
GROVE_EXIT: true
```

**Workflow planning output:**

During workflow `plan`, Grove asks the provider to emit execution-ready child task candidates through `GROVE_DECISIONS` entries shaped like:

```
GROVE_DECISIONS: ["TASK: Implement auth persistence :: Add the storage layer for issued tokens"]
```

Grove can turn those planning decisions into real child `task` beads in `br` and wire the parent feature or epic to depend on them before continuing the run.

**Checkpoint (context filling up):**

```
GROVE_CHECKPOINT: {"progress": "routes done, middleware 60%", "next_step": "finish token refresh", "context": {}, "open_questions": [], "claimed_paths": ["src/auth/**"]}
```

**Still working (prevent premature exit):**

```
GROVE_EXIT: false
```

---

## Config

Besides `grove.toml`, Grove also uses a user-owned startup prompt file. By default it lives at `.grove/startup_prompt.md`, but you can override that path with `runtime.startup_prompt_path` in `grove.toml`. The file is created by `grove init`, can be edited freely, and is injected into every new provider session before task-specific context. It is separate from `.grove/prompts/`, which stores Grove-generated rendered prompt manifests for dispatched sessions.

Claude and Codex share the same schema. The provider-specific keys are `runtime.provider`, `runtime.provider_bin`, and usually `runtime.default_model`.

```toml
# grove.toml

[runtime]
provider = "claude"         # or "codex"
provider_bin = "claude"     # selected provider CLI binary/path
default_model = "default"   # omit the provider model flag; use a concrete name to force it
workspace_root = "."
timeout_minutes = 60
startup_prompt_path = ".grove/startup_prompt.md"  # override to use a different startup prompt file
env_passthrough = []         # optional env vars forwarded to provider sessions

[scheduler]
max_parallel = 5              # parallel sessions, bounded by reservation safety
poll_interval_ms = 1000
shutdown_grace_period_ms = 1000
retry_max = 3
retry_backoff_secs = 30
critical_path_bonus = 20
ready_age_bonus_per_min = 1
retry_penalty = 10
reservation_conflict_penalty = 1000

[checkpoint]
warn_pct = 0.70               # context pressure warning
rotate_pct = 0.82             # trigger checkpoint rotation
hard_stop_pct = 0.90          # emergency kill threshold
max_context_bytes = 16000

[exit_policy]
completion_indicator_threshold = 2
heuristic_window = 8
require_explicit_exit = true

[circuit_breaker]
no_progress_threshold = 3
same_error_threshold = 5
permission_denial_threshold = 2
cooldown_minutes = 30

[memory]
db_path = ".grove/grove.db"
transcript_dir = ".grove/transcripts"
enable_playbook = true
archive_top_k = 5
max_prompt_snippets = 3
max_prompt_bullets = 12
semantic_enabled = false

[reservations]
enabled = true
default_ttl_minutes = 60

# [reactions]
# rules = []                  # optional; omit this section to keep Grove's built-in defaults

[safety]
scan_transcripts = true
inject_safety_preamble = true

[logging]
level = "info"
persist_jsonl = true
```

`grove init` writes a smaller default `grove.toml`; omitted keys keep their built-in defaults. The full schema above reflects the current config model. If you want Grove to load a different startup prompt file, set `runtime.startup_prompt_path` to another relative or absolute path.

For Codex/OpenAI workspaces, set the runtime block like this:

```toml
[runtime]
provider = "codex"
provider_bin = "codex"
default_model = "default"   # or set an explicit OpenAI model name
workspace_root = "."
timeout_minutes = 60
startup_prompt_path = ".grove/startup_prompt.md"
env_passthrough = []
```

Authentication is provider-aware:

- Claude sessions automatically receive `ANTHROPIC_API_KEY` and `CLAUDE_API_KEY` from your shell when present.
- Codex sessions automatically receive `OPENAI_API_KEY` from your shell when present.
- `env_passthrough` is only for extra non-secret environment variables you explicitly want forwarded into provider sessions.

Provider command behavior is also different:

- Claude runs `claude -p ...` and only adds `--model <name>` when `default_model` is not `"default"`.
- Codex runs `codex exec --full-auto ...` and only adds `--model <name>` when `default_model` is not `"default"`.

---

## Project Structure

```
my-project/
├── .beads/                    # br-owned task graph
│   └── issues.jsonl
├── .grove/                    # grove-owned runtime state
│   ├── grove.db               # SQLite — authoritative runtime state by default
│   ├── transcripts/           # default transcript store (configurable)
│   │   └── <bead-id>/
│   │       └── <session-id>.jsonl
│   ├── startup_prompt.md      # user-edited baseline prompt injected into new sessions
│   ├── prompts/               # grove-generated rendered prompts / manifests for dispatched sessions
│   ├── checkpoints/
│   │   └── <bead-id>/
│   │       └── <checkpoint-id>.json
│   ├── artifacts/
│   │   └── <bead-id>/
│   ├── logs/
│   └── tmp/
└── grove.toml
```

Some paths are configurable via `grove.toml`, especially `memory.db_path` and `memory.transcript_dir`.

---

## Dependencies

All required. `grove init` validates them up front and exits clearly if any are missing.

| Tool                | Purpose                                                           |
| ------------------- | ----------------------------------------------------------------- |
| `claude` or `codex` CLI | Execute provider coding sessions                               |
| `br` (beads_rust)   | Source of truth for bead state, dependencies, comments, and close sync |
| `bv` (beads_viewer) | Graph-aware triage and planning insight (`--robot-triage` and related robot views) |

That's it. No external memory or search tool is required. Grove owns archive ingest, FTS5 retrieval, handoffs, checkpoints, recovery capsules, and playbook memory natively.

---

## Roadmap

- Phase 1 — Project skeleton, beads integration kernel, `grove init` + `grove status`
- Phase 2 — Claude session runtime, protocol parser, exit policy, circuit breaker
- Phase 3 — Parallel orchestrator, checkpoint/resume, handoff persistence, file reservations, crash recovery
- Phase 4 — Native transcript archive, FTS5 search, prompt retrieval
- Phase 5 — Playbook memory, lesson ingestion, evidence scoring, prompt injection
- Phase 6 — Rich curation, diaries, anti-pattern inversion, playbook compaction

---

## License

MIT
