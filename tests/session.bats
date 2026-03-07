#!/usr/bin/env bats

# ── file-level: build binaries once ──────────────────────────────────

setup_file() {
    cd "$BATS_TEST_DIRNAME/.."
    cargo build --quiet
}

# ── per-test: isolated vexd instance ─────────────────────────────────

setup() {
    VEX="$BATS_TEST_DIRNAME/../target/debug/vex"

    TEST_TMPDIR="$(mktemp -d)"
    export VEX_SOCKET="$TEST_TMPDIR/vexd.sock"

    "$VEX" daemon &
    VEXD_PID=$!

    local i
    for i in $(seq 1 50); do
        [ -S "$VEX_SOCKET" ] && return 0
        sleep 0.1
    done
    echo "vexd failed to start within 5s" >&2
    return 1
}

teardown() {
    if [ -n "${VEXD_PID:-}" ]; then
        kill "$VEXD_PID" 2>/dev/null || true
        wait "$VEXD_PID" 2>/dev/null || true
    fi
    [ -n "${TEST_TMPDIR:-}" ] && rm -rf "$TEST_TMPDIR"
}

# ── helpers ──────────────────────────────────────────────────────────

UUID_RE='^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'

# Merge stderr so error messages from anyhow are captured in $output
vex() { "$VEX" "$@" 2>&1; }

# Poll until list matches expected pattern (max ~5s)
wait_for_no_sessions() {
    local i
    for i in $(seq 1 20); do
        result=$("$VEX" list 2>&1)
        [[ "$result" == *"no active sessions"* ]] && return 0
        sleep 0.25
    done
    return 1
}

# Attach to a session inside a PTY (via script(1)), feeding stdin from a
# subshell.  script may linger after the vex process exits because the pipe
# keeps its stdin open, so we tolerate a timeout exit code.
attach_via_pty() {
    local sid="$1"; shift
    # Remaining args are eval'd in a subshell that feeds stdin
    ( eval "$@" ) | timeout 5 script -qec "$VEX attach $sid" /dev/null 2>&1 || true
}

# ═══════════════════════════════════════════════════════════════════
#  Daemon lifecycle
# ═══════════════════════════════════════════════════════════════════

@test "vexd creates socket file on startup" {
    [ -S "$VEX_SOCKET" ]
}

@test "vexd removes socket and token on SIGTERM" {
    [ -S "$VEX_SOCKET" ]
    TOKEN_PATH="${VEX_SOCKET%.sock}.token"
    [ -f "$TOKEN_PATH" ]
    kill "$VEXD_PID"
    wait "$VEXD_PID" 2>/dev/null || true
    VEXD_PID=""
    sleep 0.2
    [ ! -e "$VEX_SOCKET" ]
    [ ! -e "$TOKEN_PATH" ]
}

@test "vexd removes socket and token on SIGINT" {
    [ -S "$VEX_SOCKET" ]
    TOKEN_PATH="${VEX_SOCKET%.sock}.token"
    [ -f "$TOKEN_PATH" ]
    kill -INT "$VEXD_PID"
    wait "$VEXD_PID" 2>/dev/null || true
    VEXD_PID=""
    sleep 0.2
    [ ! -e "$VEX_SOCKET" ]
    [ ! -e "$TOKEN_PATH" ]
}

# ═══════════════════════════════════════════════════════════════════
#  Session create
# ═══════════════════════════════════════════════════════════════════

@test "create returns a valid UUID" {
    run "$VEX" create
    [ "$status" -eq 0 ]
    [[ "$output" =~ $UUID_RE ]]
}

@test "create with --shell flag" {
    run "$VEX" create --shell /bin/sh
    [ "$status" -eq 0 ]
    [[ "$output" =~ $UUID_RE ]]
}

@test "create returns unique IDs" {
    run "$VEX" create
    ID1="$output"
    run "$VEX" create
    ID2="$output"
    [ "$ID1" != "$ID2" ]
}

@test "create --attach creates and attaches" {
    OUTPUT=$( (sleep 0.5; printf 'echo CREATEATTACH\n'; sleep 1; printf '\x1d') \
        | timeout 5 script -qec "$VEX create --attach --shell /bin/sh" /dev/null 2>&1 || true)
    [[ "$OUTPUT" == *"CREATEATTACH"* ]]
    [[ "$OUTPUT" == *"detached"* ]]
}

# ═══════════════════════════════════════════════════════════════════
#  Session list
# ═══════════════════════════════════════════════════════════════════

@test "list: no sessions" {
    run "$VEX" list
    [ "$status" -eq 0 ]
    [[ "$output" == *"no active sessions"* ]]
}

@test "bare vex defaults to list" {
    run "$VEX"
    [ "$status" -eq 0 ]
    [[ "$output" == *"no active sessions"* ]]
}

@test "ls alias works" {
    run "$VEX" ls
    [ "$status" -eq 0 ]
    [[ "$output" == *"no active sessions"* ]]
}

@test "list: header and session row" {
    run "$VEX" create
    SID="$output"

    run "$VEX" list
    [ "$status" -eq 0 ]
    [[ "$output" == *"ID"* ]]
    [[ "$output" == *"COLS"* ]]
    [[ "$output" == *"ROWS"* ]]
    [[ "$output" == *"CREATED"* ]]
    [[ "$output" == *"$SID"* ]]
}

@test "list: multiple sessions" {
    run "$VEX" create
    SID1="$output"
    run "$VEX" create
    SID2="$output"

    run "$VEX" list
    [ "$status" -eq 0 ]
    [[ "$output" == *"$SID1"* ]]
    [[ "$output" == *"$SID2"* ]]
}

@test "list: default dimensions are 80x24" {
    "$VEX" create >/dev/null

    run "$VEX" list
    [ "$status" -eq 0 ]
    [[ "$output" == *"80"* ]]
    [[ "$output" == *"24"* ]]
}

# ═══════════════════════════════════════════════════════════════════
#  Session kill
# ═══════════════════════════════════════════════════════════════════

@test "kill by full UUID" {
    run "$VEX" create
    SID="$output"

    run "$VEX" kill "$SID"
    [ "$status" -eq 0 ]
    [[ "$output" == *"killed session"* ]]

    wait_for_no_sessions
}

@test "kill by UUID prefix" {
    run "$VEX" create
    SID="$output"
    PREFIX="${SID:0:8}"

    run "$VEX" kill "$PREFIX"
    [ "$status" -eq 0 ]
    [[ "$output" == *"killed session $SID"* ]]
}

@test "kill: nonexistent UUID fails" {
    run vex kill "00000000-0000-0000-0000-000000000000"
    [ "$status" -ne 0 ]
}

@test "kill: no matching prefix fails" {
    "$VEX" create >/dev/null

    run vex kill "zzzzz"
    [ "$status" -ne 0 ]
    [[ "$output" == *"no session matching"* ]]
}

@test "kill: ambiguous prefix fails" {
    for _ in $(seq 1 16); do
        "$VEX" create >/dev/null
    done

    # Empty prefix matches every session
    run vex kill ""
    [ "$status" -ne 0 ]
    [[ "$output" == *"ambiguous"* ]]
}

@test "killing all sessions leaves list empty" {
    run "$VEX" create
    SID1="$output"
    run "$VEX" create
    SID2="$output"

    "$VEX" kill "$SID1"
    "$VEX" kill "$SID2"

    wait_for_no_sessions
}

# ═══════════════════════════════════════════════════════════════════
#  Prefix resolution
# ═══════════════════════════════════════════════════════════════════

@test "single-char prefix resolves when only one session exists" {
    run "$VEX" create
    SID="$output"
    PREFIX="${SID:0:1}"

    run "$VEX" kill "$PREFIX"
    [ "$status" -eq 0 ]
}

@test "4-char prefix resolves to correct session" {
    run "$VEX" create
    SID="$output"
    PREFIX="${SID:0:4}"

    run "$VEX" kill "$PREFIX"
    [ "$status" -eq 0 ]
    [[ "$output" == *"killed session $SID"* ]]
}

# ═══════════════════════════════════════════════════════════════════
#  Attach / detach
# ═══════════════════════════════════════════════════════════════════

@test "attach and detach with Ctrl+]" {
    run "$VEX" create
    [ "$status" -eq 0 ]
    SID="$output"

    OUTPUT=$(attach_via_pty "$SID" "sleep 1; printf '\x1d'")
    [[ "$OUTPUT" == *"detached"* ]]

    # Session should still be alive after detach
    run "$VEX" list
    [[ "$output" == *"$SID"* ]]
}

@test "session executes commands and streams output" {
    run "$VEX" create --shell /bin/sh
    [ "$status" -eq 0 ]
    SID="$output"

    OUTPUT=$(attach_via_pty "$SID" "sleep 0.5; printf 'echo VEXMARKER42\n'; sleep 1; printf '\x1d'")
    [[ "$OUTPUT" == *"VEXMARKER42"* ]]
}

@test "session persists across attach/detach cycles" {
    run "$VEX" create --shell /bin/sh
    [ "$status" -eq 0 ]
    SID="$output"

    # First attach: create a file inside the session
    attach_via_pty "$SID" "sleep 0.5; printf 'touch $TEST_TMPDIR/proof\n'; sleep 1; printf '\x1d'"

    # File should exist (proves the command ran)
    [ -f "$TEST_TMPDIR/proof" ]

    # Second attach: verify session is still the same shell
    OUTPUT=$(attach_via_pty "$SID" "sleep 0.5; printf 'ls $TEST_TMPDIR/proof\n'; sleep 1; printf '\x1d'")
    [[ "$OUTPUT" == *"proof"* ]]
}

# ═══════════════════════════════════════════════════════════════════
#  Error handling
# ═══════════════════════════════════════════════════════════════════

@test "client errors when daemon is not running" {
    kill "$VEXD_PID" 2>/dev/null || true
    wait "$VEXD_PID" 2>/dev/null || true
    VEXD_PID=""
    sleep 0.2

    run "$VEX" list
    [ "$status" -ne 0 ]
}

@test "client errors with bad socket path" {
    export VEX_SOCKET="/tmp/nonexistent/deep/path/vexd.sock"
    run "$VEX" list
    [ "$status" -ne 0 ]
}

# ═══════════════════════════════════════════════════════════════════
#  Authentication
# ═══════════════════════════════════════════════════════════════════

@test "daemon writes token file on startup" {
    TOKEN_PATH="${VEX_SOCKET%.sock}.token"
    [ -f "$TOKEN_PATH" ]
    [ -n "$(cat "$TOKEN_PATH")" ]
}

@test "token file has restrictive permissions" {
    TOKEN_PATH="${VEX_SOCKET%.sock}.token"
    PERMS=$(stat -c '%a' "$TOKEN_PATH")
    [ "$PERMS" = "600" ]
}

@test "wrong token is rejected" {
    run env VEX_TOKEN="bad-token-value" "$VEX" list
    [ "$status" -ne 0 ]
    [[ "$output" == *"invalid token"* ]]
}

@test "explicit correct token works" {
    TOKEN_PATH="${VEX_SOCKET%.sock}.token"
    TOKEN=$(cat "$TOKEN_PATH")
    run "$VEX" --token "$TOKEN" list
    [ "$status" -eq 0 ]
}

# ═══════════════════════════════════════════════════════════════════
#  Daemon resilience
# ═══════════════════════════════════════════════════════════════════

@test "daemon serves multiple sequential clients" {
    run "$VEX" list
    [ "$status" -eq 0 ]
    run "$VEX" list
    [ "$status" -eq 0 ]
    run "$VEX" list
    [ "$status" -eq 0 ]
}

@test "daemon handles rapid session creation" {
    for _ in $(seq 1 5); do
        run "$VEX" create
        [ "$status" -eq 0 ]
        [[ "$output" =~ $UUID_RE ]]
    done

    run "$VEX" list
    [ "$status" -eq 0 ]
    local count
    count=$(echo "$output" | grep -c '[0-9a-f]\{8\}-')
    [ "$count" -eq 5 ]
}

@test "vexd terminates all sessions on shutdown" {
    "$VEX" create >/dev/null
    "$VEX" create >/dev/null

    kill "$VEXD_PID"
    wait "$VEXD_PID" 2>/dev/null || true
    VEXD_PID=""
    sleep 0.2

    [ ! -e "$VEX_SOCKET" ]
}

# ═══════════════════════════════════════════════════════════════════
#  TCP transport
# ═══════════════════════════════════════════════════════════════════

@test "tcp: create and list over TCP" {
    # Restart daemon with --listen on a random port
    kill "$VEXD_PID" 2>/dev/null || true
    wait "$VEXD_PID" 2>/dev/null || true

    TCP_PORT=$((20000 + RANDOM % 10000))
    "$VEX" daemon --listen "127.0.0.1:$TCP_PORT" &
    VEXD_PID=$!

    local i
    for i in $(seq 1 50); do
        [ -S "$VEX_SOCKET" ] && break
        sleep 0.1
    done

    TOKEN=$(cat "${VEX_SOCKET%.sock}.token")

    # Create a session via TCP
    run "$VEX" --connect "127.0.0.1:$TCP_PORT" --token "$TOKEN" create
    [ "$status" -eq 0 ]
    [[ "$output" =~ $UUID_RE ]]
    SID="$output"

    # List sessions via TCP
    run "$VEX" --connect "127.0.0.1:$TCP_PORT" --token "$TOKEN" list
    [ "$status" -eq 0 ]
    [[ "$output" == *"$SID"* ]]

    # Kill session via TCP
    run "$VEX" --connect "127.0.0.1:$TCP_PORT" --token "$TOKEN" kill "$SID"
    [ "$status" -eq 0 ]
    [[ "$output" == *"killed session"* ]]
}

@test "tcp: --connect without --token gives clear error" {
    run "$VEX" --connect "127.0.0.1:9999" list
    [ "$status" -ne 0 ]
    [[ "$output" == *"--token"* ]]
    [[ "$output" == *"VEX_TOKEN"* ]]
}

@test "tcp: wrong token rejected over TCP" {
    kill "$VEXD_PID" 2>/dev/null || true
    wait "$VEXD_PID" 2>/dev/null || true

    TCP_PORT=$((20000 + RANDOM % 10000))
    "$VEX" daemon --listen "127.0.0.1:$TCP_PORT" &
    VEXD_PID=$!

    local i
    for i in $(seq 1 50); do
        [ -S "$VEX_SOCKET" ] && break
        sleep 0.1
    done

    run "$VEX" --connect "127.0.0.1:$TCP_PORT" --token "wrong-token" list
    [ "$status" -ne 0 ]
    [[ "$output" == *"invalid token"* ]]
}
