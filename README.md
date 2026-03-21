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

Define your tasks with `br`. Type `grove run`. Walk away. Come back to completed work — sessions handled, context rotations managed, and native handoffs, transcript archive, and playbook memory carried forward automatically. Anything that failed or couldn't mirror back to `br` is flagged, not silently lost.

---

## Standing on Shoulders

Grove didn't appear from nothing. The exit gate and circuit breaker come from [Frank Bria](https://github.com/frankbria)'s [ralph-claude-code](https://github.com/frankbria/ralph-claude-code), which proved that an autonomous Claude loop needs both heuristic detection and an explicit exit signal to avoid premature stops. The entire task graph — dependencies, lifecycle, ready-queue — runs on [Jeff Emanuel](https://github.com/Dicklesworthstone)'s [beads_rust](https://github.com/Dicklesworthstone/beads_rust), and grove uses his [beads_viewer](https://github.com/Dicklesworthstone/beads_viewer) for PageRank, critical path, and triage scoring when deciding which bead to dispatch next. Grove's native transcript archive is modeled after his [coding_agent_session_search](https://github.com/Dicklesworthstone/coding_agent_session_search), and the playbook engine — evidence scoring, confidence decay, curation, anti-pattern inversion — is adapted directly from his [cass_memory_system](https://github.com/Dicklesworthstone/cass_memory_system). His [ntm](https://github.com/Dicklesworthstone/ntm) shaped early thinking about session layout and parallel coordination, though grove chose direct process spawning over tmux. Finally, [nwiizo](https://github.com/nwiizo)'s [ccswarm](https://github.com/nwiizo/ccswarm) influenced the Rust workspace structure, type-state patterns, and task scoring design. Thank you all.

---

## Workflow

```
You                         br / bv                        grove                          Claude
 │                            │                              │                              │
 ├─ br init                   │                              │                              │
 ├─ br create "schema"  ──────▶ issues.jsonl                  │                              │
 ├─ br create "auth"    ──────▶ issues.jsonl                  │                              │
 ├─ br dep add auth schema ───▶ dependency recorded          │                              │
 │                            │                              │                              │
 ├─ grove init          ─────────────────────────────────────▶ .grove/grove.db created       │
 ├─ grove run           ─────────────────────────────────────▶ orchestrator starts           │
 │                            │                              │                              │
 │  ┌─────────────────── coordinator loop ──────────────────────────────────────────┐       │
 │  │                         │                              │                      │       │
 │  │  sync ──────────────────▶ br ready --json              │                      │       │
 │  │                         ◀── [bd-e9b1d4]                │                      │       │
 │  │                         │                              │                      │       │
 │  │  score ─────────────────▶ bv --robot-triage    ────────▶ rank by priority     │       │
 │  │                         ◀── critical path, PageRank    │  + bv bonuses        │       │
 │  │                                                        │  + reservation check │       │
 │  │                                                        │                      │       │
 │  │  dispatch ─────────────────────────────────────────────▶ build prompt         │       │
 │  │                                                        │  (task + handoffs    │       │
 │  │                                                        │   + archive snippets │       │
 │  │                                                        │   + playbook rules)  │       │
 │  │                                                        │                      │       │
 │  │                                                        ├── claude -p "..."  ──▶ works │
 │  │                                                        │                      ◀── stdout
 │  │                                                        │                      │       │
 │  │                                                        │  parse protocol:     │       │
 │  │                                                        │   GROVE_RESULT       │       │
 │  │                                                        │   GROVE_ARTIFACTS    │       │
 │  │                                                        │   GROVE_LESSONS      │       │
 │  │                                                        │   GROVE_EXIT: true   │       │
 │  │                                                        │                      │       │
 │  │  on success:                                           │                      │       │
 │  │   persist handoff ─────────────────────────────────────▶ grove.db             │       │
 │  │   index transcript ────────────────────────────────────▶ FTS5 archive         │       │
 │  │   extract lessons ─────────────────────────────────────▶ playbook             │       │
 │  │   mirror ──────────────▶ br close bd-e9b1d4            │                      │       │
 │  │                         │                              │                      │       │
 │  │  on context pressure:                                  │                      │       │
 │  │   GROVE_CHECKPOINT ────────────────────────────────────▶ persist checkpoint   │       │
 │  │   spawn new session ───────────────────────────────────▶ claude -p "resume…" ─▶ works │
 │  │                                                        │                      │       │
 │  │  on stuck loop:                                        │                      │       │
 │  │   circuit breaker OPEN ────────────────────────────────▶ cooldown 30min       │       │
 │  │   half-open test   ────────────────────────────────────▶ retry one iteration  │       │
 │  │                                                        │                      │       │
 │  │  next tick: br ready returns newly unblocked children  │                      │       │
 │  │  repeat until all beads done                           │                      │       │
 │  └───────────────────────────────────────────────────────────────────────────────┘       │
 │                            │                              │                              │
 ◀── grove status shows results (succeeded / failed / mirror-pending)                │                              │
 │                            │                              │                              │
```

---

## How It Works

Grove runs a continuous autonomous loop over your beads task graph. Each bead is dispatched to a Claude session. The coordinator can keep multiple sessions in flight concurrently up to `max_parallel`, while still enforcing file reservation safety and a single active leader lease. When context exhausts, grove checkpoints and spawns a fresh session automatically. Child beads inherit structured handoffs from parents.

### The Loop

```
grove run
  │
  ├─ sync br ready --json
  │     → [bd-e9b1d4, bd-7f3a2c]  (no blockers, both ready)
  │
  ├─ score candidates (priority + critical path + bv insights)
  │
  ├─ dispatch top-scoring beads (up to max_parallel)
  │     session A: claude -p "<task + parent handoffs + archive snippets + playbook rules>"
  │     session B: claude -p "<task + parent handoffs + archive snippets + playbook rules>"
  │
  ├─ session A outputs GROVE_EXIT: true (+ completion indicators met)
  │     → persist handoff
  │     → index transcript into grove's native archive
  │     → extract lessons into playbook
  │     → mirror to br (close + comment)
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
cargo install --git https://github.com/quangdang46/grove
```

**Required tools (install first):**

- `claude` CLI — [https://claude.ai/code](https://claude.ai/code)
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

br dep add bd-7f3a2c bd-e9b1d4   # auth depends on schema

# Init grove
grove init

# Run — then go do something else
grove run
```

Grove handles everything from here. When it finishes, your beads are closed and mirrored back to `br`. If a mirror fails, grove preserves the local result and flags it for retry — run `grove status` to see what landed and what needs attention.

---

## Usage

```bash
# Init grove workspace
grove init

# Start orchestrator (the main command)
grove run

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

Grove communicates with Claude sessions through stdout markers:

**Task complete:**

```
GROVE_RESULT: Implemented JWT auth middleware with refresh token support
GROVE_ARTIFACTS: ["src/middleware/auth.rs", "tests/auth_test.rs"]
GROVE_LESSONS: ["Always validate token expiry before checking signature"]
GROVE_DECISIONS: ["Used RS256 for token signing"]
GROVE_WARNINGS: ["Rate limiting not yet implemented"]
GROVE_EXIT: true
```

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

```toml
# grove.toml

[runtime]
claude_bin = "claude"
default_model = "sonnet"
workspace_root = "."
timeout_minutes = 60

[scheduler]
max_parallel = 5              # parallel sessions, bounded by reservation safety
poll_interval_ms = 1000
retry_max = 3
retry_backoff_secs = 30
critical_path_bonus = 20
reservation_conflict_penalty = 1000

[checkpoint]
warn_pct = 0.70               # context pressure warning
rotate_pct = 0.82             # trigger checkpoint rotation
hard_stop_pct = 0.90          # emergency kill threshold

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
enable_playbook = true
archive_top_k = 5
max_prompt_snippets = 3
max_prompt_bullets = 12

[reservations]
enabled = true
default_ttl_minutes = 60

[safety]
scan_transcripts = true
inject_safety_preamble = true

[logging]
level = "info"
persist_jsonl = true
```

---

## Project Structure

```
my-project/
├── .beads/                    # br-owned task graph
│   └── issues.jsonl
├── .grove/                    # grove-owned runtime state
│   ├── grove.db               # SQLite — authoritative runtime state
│   ├── config.snapshot.json
│   ├── lock/
│   │   └── leader.lock        # single-coordinator enforcement
│   ├── transcripts/
│   │   └── <bead-id>/
│   │       └── <session-id>.jsonl
│   ├── checkpoints/
│   │   └── <bead-id>/
│   │       └── <checkpoint-id>.json
│   ├── artifacts/
│   │   └── <bead-id>/
│   ├── logs/
│   │   └── orchestrator.jsonl
│   └── tmp/
└── grove.toml
```

---

## Dependencies

All required. `grove init` validates them up front and exits clearly if any are missing.

| Tool                | Purpose                                                           |
| ------------------- | ----------------------------------------------------------------- |
| `claude` CLI        | Execute Claude coding sessions                                    |
| `br` (beads_rust)   | Source of truth for bead state, dependencies, and close/update sync |
| `bv` (beads_viewer) | Graph-aware triage and planning insight                            |

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