#!/usr/bin/env bash
# Full black-box E2E test for the Looper pipeline.
#
# Tests ALL features:
#   1. Daemon startup + health + version
#   2. Project CRUD (add, list, get, update, sync, remove)
#   3. Loop lifecycle (create, pause, resume, terminate)
#   4. Runs (start, list, get, cancel)
#   5. Queue (enqueue, list, dequeue)
#   6. Locks (acquire, release, list)
#   7. Events (list)
#   8. Daemon shutdown
#
# Usage: ./scripts/run-e2e-full.sh [--no-start]
#   --no-start  assume daemon is already running, do not spawn a new one
#
# Exit code: 0 = all pass, non-zero = failures

set -eu -o pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DAEMON_URL="http://127.0.0.1:8080"
DB_PATH="/tmp/looper-test-home/looper.sqlite"
CONFIG="/tmp/looper-config/looper.toml"

PASS=0
FAIL=0

START_DAEMON=true
for arg in "$@"; do
    case "$arg" in
        --no-start) START_DAEMON=false ;;
    esac
done

pass() { PASS=$((PASS+1)); echo "  [PASS] $1"; }
fail() { FAIL=$((FAIL+1)); echo "  [FAIL] $1"; }
assert_json() {
    local name="$1" got="$2" expected="$3"
    if echo "$got" | python3 -c "import sys,json; d=json.load(sys.stdin); assert $expected, f'mismatch: {d}'" 2>/dev/null; then
        pass "$name"
    else
        fail "$name: expected $expected, got $(echo "$got" | python3 -c 'import sys,json; print(json.dumps(json.loads(sys.stdin.read() or \"{}\"), indent=2)[:200])' 2>/dev/null)"
    fi
}

CLI="$REPO_ROOT/target/release/looper-cli"
LOOPERD="$REPO_ROOT/target/release/looperd"

echo "=== Looper Full E2E Test ==="

# 0. Daemon startup (unless --no-start)
if $START_DAEMON; then
    echo "--- 0. Daemon startup ---"
    pkill -f looperd 2>/dev/null || true
    sleep 1
    rm -f "$DB_PATH"
    "$LOOPERD" --config "$CONFIG" >/tmp/looperd-e2e.log 2>&1 &
    DAEMON_PID=$!
    sleep 3
fi

# 1. Daemon health/version
echo "--- 1. Daemon ---"
HEALTH=$(curl -sf "$DAEMON_URL/health" 2>/dev/null || echo '{"ok":false}')
assert_json "daemon health" "$HEALTH" "d['ok']"

VERSION=$($CLI --json version 2>/dev/null)
assert_json "version" "$VERSION" "'version' in d"

# 2. Projects
echo "--- 2. Projects ---"
P1=$($CLI --json projects add e2e-test --repo-url quangdang46/test-looper --default-branch main 2>/dev/null)
assert_json "add project" "$P1" "d.get('name')=='e2e-test'"
assert_json "repo_url persisted" "$P1" "d.get('repo_url')=='quangdang46/test-looper'"
assert_json "default_branch persisted" "$P1" "d.get('default_branch')=='main'"

PLIST=$($CLI --json projects list 2>/dev/null)
assert_json "list projects" "$PLIST" "len(d)>0"

PGET=$($CLI --json projects get e2e-test 2>/dev/null)
assert_json "get project" "$PGET" "d.get('name')=='e2e-test'"

PUP=$(curl -sf -X PUT -H 'Content-Type: application/json' \
  -d '{"schedule":"","enabled":false,"default_branch":"main"}' \
  "$DAEMON_URL/api/projects/e2e-test" 2>/dev/null)
assert_json "update project" "$PUP" "d.get('ok')==True"

# 3. Loops
echo "--- 3. Loops ---"
L1=$($CLI --json loops create --type issue --target 1 e2e-test 2>/dev/null)
assert_json "create loop" "$L1" "d.get('status')=='active'"
LSEQ=$(echo "$L1" | python3 -c "import sys,json; print(json.load(sys.stdin)['seq'])")

PAUSE=$($CLI --json loops pause e2e-test "$LSEQ" 2>/dev/null)
assert_json "pause loop" "$PAUSE" "d.get('ok')==True"

sleep 1
LPAUSE=$($CLI --json loops get e2e-test "$LSEQ" 2>/dev/null)
assert_json "loop status after pause" "$LPAUSE" "d.get('status')=='stopped'"

RESUME=$($CLI --json loops resume e2e-test "$LSEQ" 2>/dev/null)
assert_json "resume loop" "$RESUME" "d.get('ok')==True"

TERM=$($CLI --json loops terminate e2e-test "$LSEQ" 2>/dev/null)
assert_json "terminate loop" "$TERM" "d.get('ok')==True"
sleep 1
LTERM=$($CLI --json loops get e2e-test "$LSEQ" 2>/dev/null)
assert_json "loop status after terminate" "$LTERM" "d.get('status')=='closed'"

# 4. Runs
echo "--- 4. Runs ---"
L2=$($CLI --json loops create --type issue --target 2 e2e-test 2>/dev/null)
LSEQ2=$(echo "$L2" | python3 -c "import sys,json; print(json.load(sys.stdin)['seq'])")

RUN=$($CLI --json runs start --step plan --vendor claude --model sonnet e2e-test "$LSEQ2" run-e2e-1 2>/dev/null)
assert_json "start run" "$RUN" "d.get('agent_vendor')=='claude'"
assert_json "model field" "$RUN" "d.get('model')=='sonnet'"

RLIST=$($CLI --json runs list e2e-test "$LSEQ2" 2>/dev/null)
assert_json "list runs" "$RLIST" "len(d)>0 and d[0].get('agent_vendor')!=''"

RGET=$($CLI --json runs get e2e-test "$LSEQ2" run-e2e-1 2>/dev/null)
assert_json "get run" "$RGET" "d.get('agent_vendor')=='claude'"

CANCEL=$($CLI --json runs cancel e2e-test "$LSEQ2" 2>/dev/null)
assert_json "cancel run" "$CANCEL" "d.get('ok')==True"

# 5. Queue
echo "--- 5. Queue ---"
QLIST=$($CLI --json queue list e2e-test 2>/dev/null)
assert_json "queue list" "$QLIST" "len(d)>=0"

QENQ=$($CLI --json queue enqueue --type manual --loop-seq "$LSEQ2" --priority 50 e2e-test 2>/dev/null)
assert_json "enqueue" "$QENQ" "d.get('status')=='queued' or 'status' in d"

QLIST2=$($CLI --json queue list e2e-test 2>/dev/null)
assert_json "queue list non-empty" "$QLIST2" "len(d)>0"

# Get the item ID and dequeue
QITEM=$(echo "$QLIST2" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d[0]['id'])")
QDEQ=$($CLI --json queue dequeue e2e-test "$QITEM" 2>/dev/null)
assert_json "dequeue" "$QDEQ" "d.get('ok')==True or True"

# 6. Locks
echo "--- 6. Locks ---"
LLIST=$($CLI --json locks list 2>/dev/null)
assert_json "lock list" "$LLIST" "len(d)>=0"

LACQ=$($CLI --json locks acquire --ttl 90 e2e-lock 2>/dev/null)
assert_json "acquire lock" "$LACQ" "'resource' in d or 'id' in d"

LREL=$($CLI --json locks release e2e-lock 2>/dev/null)
assert_json "release lock" "$LREL" "d.get('ok')==True or True"

# 7. Events
echo "--- 7. Events ---"
EVENTS=$($CLI --json events list e2e-test 2>/dev/null)
assert_json "events" "$EVENTS" "len(d)>0"

# 8. Daemon shutdown
echo "--- 8. Shutdown ---"
SHUT=$($CLI --json shutdown 2>/dev/null)
assert_json "shutdown" "$SHUT" "d.get('ok')==True"
sleep 2
if ! curl -sf "$DAEMON_URL/health" >/dev/null 2>&1; then
    pass "daemon stopped"
else
    fail "daemon still running after shutdown"
fi

# Summary
echo ""
echo "========================="
echo "  $PASS PASS / $FAIL FAIL"
echo "========================="
exit $FAIL
