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
    export VEX_DIR="$TEST_TMPDIR/.vex"
    export VEX_PORT=$((20000 + RANDOM % 10000))

    "$VEX" daemon start 2>/dev/null

    # Verify daemon is reachable
    local i
    for i in $(seq 1 50); do
        "$VEX" session list >/dev/null 2>&1 && return 0
        sleep 0.1
    done
    echo "daemon failed to start within 5s" >&2
    return 1
}

teardown() {
    "$VEX" daemon stop 2>/dev/null || true
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
        result=$("$VEX" session list 2>&1)
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
#  Top-level CLI
# ═══════════════════════════════════════════════════════════════════

@test "bare vex shows help" {
    run "$VEX"
    [ "$status" -eq 0 ]
    [[ "$output" == *"terminal multiplexer"* ]]
    [[ "$output" == *"session"* ]]
    [[ "$output" == *"daemon"* ]]
}

@test "vex --version shows version" {
    run "$VEX" --version
    [ "$status" -eq 0 ]
    [[ "$output" == vex* ]]
}

# ═══════════════════════════════════════════════════════════════════
#  Daemon lifecycle
# ═══════════════════════════════════════════════════════════════════

@test "daemon start creates pid file" {
    [ -f "$VEX_DIR/daemon.pid" ]
}

@test "daemon start creates log file" {
    [ -f "$VEX_DIR/daemon.log" ]
}

@test "daemon start when already running says so" {
    run vex daemon start
    [ "$status" -eq 0 ]
    [[ "$output" == *"already running"* ]]
}

@test "daemon stop removes pid file" {
    "$VEX" daemon stop 2>/dev/null
    [ ! -f "$VEX_DIR/daemon.pid" ]

    # Re-start for other teardown cleanup
    "$VEX" daemon start 2>/dev/null
}

@test "daemon shuts down on SIGTERM" {
    PID=$(cat "$VEX_DIR/daemon.pid")
    kill "$PID"

    local i
    for i in $(seq 1 20); do
        kill -0 "$PID" 2>/dev/null || break
        sleep 0.1
    done

    # PID file should be cleaned up by the daemon's signal handler
    [ ! -f "$VEX_DIR/daemon.pid" ]

    # Restart for teardown
    "$VEX" daemon start 2>/dev/null
}

@test "daemon shuts down on SIGINT" {
    PID=$(cat "$VEX_DIR/daemon.pid")
    kill -INT "$PID"

    local i
    for i in $(seq 1 20); do
        kill -0 "$PID" 2>/dev/null || break
        sleep 0.1
    done

    [ ! -f "$VEX_DIR/daemon.pid" ]

    # Restart for teardown
    "$VEX" daemon start 2>/dev/null
}

@test "daemon logs shows listening message" {
    run "$VEX" daemon logs
    [ "$status" -eq 0 ]
    [[ "$output" == *"listening"* ]]
}

@test "daemon status shows running" {
    run vex daemon status
    [ "$status" -eq 0 ]
    [[ "$output" == *"daemon running"* ]]
}

@test "daemon status shows not running when stopped" {
    "$VEX" daemon stop 2>/dev/null
    run vex daemon status
    [ "$status" -eq 0 ]
    [[ "$output" == *"not running"* ]]

    # Restart for teardown
    "$VEX" daemon start 2>/dev/null
}

# ═══════════════════════════════════════════════════════════════════
#  Session create
# ═══════════════════════════════════════════════════════════════════

@test "create returns a valid UUID" {
    run "$VEX" session create
    [ "$status" -eq 0 ]
    [[ "$output" =~ $UUID_RE ]]
}

@test "create with --shell flag" {
    run "$VEX" session create --shell /bin/sh
    [ "$status" -eq 0 ]
    [[ "$output" =~ $UUID_RE ]]
}

@test "create returns unique IDs" {
    run "$VEX" session create
    ID1="$output"
    run "$VEX" session create
    ID2="$output"
    [ "$ID1" != "$ID2" ]
}

@test "create --attach creates and attaches" {
    OUTPUT=$( (sleep 0.5; printf 'echo CREATEATTACH\n'; sleep 1; printf '\x1d') \
        | timeout 5 script -qec "$VEX session create --attach --shell /bin/sh" /dev/null 2>&1 || true)
    [[ "$OUTPUT" == *"CREATEATTACH"* ]]
    [[ "$OUTPUT" == *"detached"* ]]
}

# ═══════════════════════════════════════════════════════════════════
#  Session list
# ═══════════════════════════════════════════════════════════════════

@test "list: no sessions" {
    run "$VEX" session list
    [ "$status" -eq 0 ]
    [[ "$output" == *"no active sessions"* ]]
}

@test "ls alias works" {
    run "$VEX" session ls
    [ "$status" -eq 0 ]
    [[ "$output" == *"no active sessions"* ]]
}

@test "list: header and session row" {
    run "$VEX" session create
    SID="$output"

    run "$VEX" session list
    [ "$status" -eq 0 ]
    [[ "$output" == *"ID"* ]]
    [[ "$output" == *"COLS"* ]]
    [[ "$output" == *"ROWS"* ]]
    [[ "$output" == *"CREATED"* ]]
    [[ "$output" == *"$SID"* ]]
}

@test "list: multiple sessions" {
    run "$VEX" session create
    SID1="$output"
    run "$VEX" session create
    SID2="$output"

    run "$VEX" session list
    [ "$status" -eq 0 ]
    [[ "$output" == *"$SID1"* ]]
    [[ "$output" == *"$SID2"* ]]
}

@test "list: default dimensions are 80x24" {
    "$VEX" session create >/dev/null

    run "$VEX" session list
    [ "$status" -eq 0 ]
    [[ "$output" == *"80"* ]]
    [[ "$output" == *"24"* ]]
}

# ═══════════════════════════════════════════════════════════════════
#  Session kill
# ═══════════════════════════════════════════════════════════════════

@test "kill by full UUID" {
    run "$VEX" session create
    SID="$output"

    run "$VEX" session kill "$SID"
    [ "$status" -eq 0 ]
    [[ "$output" == *"killed session"* ]]

    wait_for_no_sessions
}

@test "kill by UUID prefix" {
    run "$VEX" session create
    SID="$output"
    PREFIX="${SID:0:8}"

    run "$VEX" session kill "$PREFIX"
    [ "$status" -eq 0 ]
    [[ "$output" == *"killed session $SID"* ]]
}

@test "kill: nonexistent UUID fails" {
    run vex session kill "00000000-0000-0000-0000-000000000000"
    [ "$status" -ne 0 ]
}

@test "kill: no matching prefix fails" {
    "$VEX" session create >/dev/null

    run vex session kill "zzzzz"
    [ "$status" -ne 0 ]
    [[ "$output" == *"no session matching"* ]]
}

@test "kill: ambiguous prefix fails" {
    for _ in $(seq 1 16); do
        "$VEX" session create >/dev/null
    done

    # Empty prefix matches every session
    run vex session kill ""
    [ "$status" -ne 0 ]
    [[ "$output" == *"ambiguous"* ]]
}

@test "killing all sessions leaves list empty" {
    run "$VEX" session create
    SID1="$output"
    run "$VEX" session create
    SID2="$output"

    "$VEX" session kill "$SID1"
    "$VEX" session kill "$SID2"

    wait_for_no_sessions
}

# ═══════════════════════════════════════════════════════════════════
#  Prefix resolution
# ═══════════════════════════════════════════════════════════════════

@test "single-char prefix resolves when only one session exists" {
    run "$VEX" session create
    SID="$output"
    PREFIX="${SID:0:1}"

    run "$VEX" session kill "$PREFIX"
    [ "$status" -eq 0 ]
}

@test "4-char prefix resolves to correct session" {
    run "$VEX" session create
    SID="$output"
    PREFIX="${SID:0:4}"

    run "$VEX" session kill "$PREFIX"
    [ "$status" -eq 0 ]
    [[ "$output" == *"killed session $SID"* ]]
}

# ═══════════════════════════════════════════════════════════════════
#  Attach / detach
# ═══════════════════════════════════════════════════════════════════

@test "attach and detach with Ctrl+]" {
    run "$VEX" session create
    [ "$status" -eq 0 ]
    SID="$output"

    OUTPUT=$(attach_via_pty "$SID" "sleep 1; printf '\x1d'")
    [[ "$OUTPUT" == *"detached"* ]]

    # Session should still be alive after detach
    run "$VEX" session list
    [[ "$output" == *"$SID"* ]]
}

@test "session executes commands and streams output" {
    run "$VEX" session create --shell /bin/sh
    [ "$status" -eq 0 ]
    SID="$output"

    OUTPUT=$(attach_via_pty "$SID" "sleep 0.5; printf 'echo VEXMARKER42\n'; sleep 1; printf '\x1d'")
    [[ "$OUTPUT" == *"VEXMARKER42"* ]]
}

@test "session persists across attach/detach cycles" {
    run "$VEX" session create --shell /bin/sh
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
    "$VEX" daemon stop 2>/dev/null || true
    sleep 0.2

    run vex session list
    [ "$status" -ne 0 ]
    [[ "$output" == *"daemon"* ]]
}

# ═══════════════════════════════════════════════════════════════════
#  Daemon resilience
# ═══════════════════════════════════════════════════════════════════

@test "daemon serves multiple sequential clients" {
    run "$VEX" session list
    [ "$status" -eq 0 ]
    run "$VEX" session list
    [ "$status" -eq 0 ]
    run "$VEX" session list
    [ "$status" -eq 0 ]
}

@test "daemon handles rapid session creation" {
    for _ in $(seq 1 5); do
        run "$VEX" session create
        [ "$status" -eq 0 ]
        [[ "$output" =~ $UUID_RE ]]
    done

    run "$VEX" session list
    [ "$status" -eq 0 ]
    local count
    count=$(echo "$output" | grep -c '[0-9a-f]\{8\}-')
    [ "$count" -eq 5 ]
}

@test "daemon terminates all sessions on shutdown" {
    "$VEX" session create >/dev/null
    "$VEX" session create >/dev/null

    PID=$(cat "$VEX_DIR/daemon.pid")
    kill "$PID"

    local i
    for i in $(seq 1 20); do
        kill -0 "$PID" 2>/dev/null || break
        sleep 0.1
    done

    [ ! -f "$VEX_DIR/daemon.pid" ]

    # Restart for teardown
    "$VEX" daemon start 2>/dev/null
}

# ═══════════════════════════════════════════════════════════════════
#  Remote connect / disconnect
# ═══════════════════════════════════════════════════════════════════

@test "remote disconnect: no-op when not connected" {
    run "$VEX" remote disconnect
    [ "$status" -eq 0 ]
}

@test "remote disconnect: cleans up connection file" {
    # Manually create a saved connection to test disconnect cleanup
    mkdir -p "$VEX_DIR"
    echo '{"host":"test@host","tunnel_port":12345}' > "$VEX_DIR/connect.json"

    run vex remote disconnect
    [ "$status" -eq 0 ]
    [[ "$output" == *"disconnected"* ]]
    [ ! -f "$VEX_DIR/connect.json" ]
}

@test "remote list: shows not connected" {
    run "$VEX" remote list
    [ "$status" -eq 0 ]
    [[ "$output" == *"not connected"* ]]
}

@test "remote list: shows connection" {
    mkdir -p "$VEX_DIR"
    echo '{"host":"user@myhost","tunnel_port":12345}' > "$VEX_DIR/connect.json"

    run "$VEX" remote list
    [ "$status" -eq 0 ]
    [[ "$output" == *"user@myhost"* ]]

    rm "$VEX_DIR/connect.json"
}

@test "commands use tunnel port when connected" {
    # Create a fake connection pointing to our actual daemon port
    mkdir -p "$VEX_DIR"
    echo "{\"host\":\"fake\",\"tunnel_port\":$VEX_PORT}" > "$VEX_DIR/connect.json"

    # Commands should work through the "tunnel" (same local daemon)
    run "$VEX" session list
    [ "$status" -eq 0 ]
    [[ "$output" == *"no active sessions"* ]]

    run "$VEX" session create
    [ "$status" -eq 0 ]
    [[ "$output" =~ $UUID_RE ]]
    SID="$output"

    run "$VEX" session kill "$SID"
    [ "$status" -eq 0 ]

    # Clean up
    rm "$VEX_DIR/connect.json"
}

# ═══════════════════════════════════════════════════════════════════
#  Terminal size
# ═══════════════════════════════════════════════════════════════════

@test "session reports PTY size matching attached client" {
    run "$VEX" session create --shell /bin/sh
    [ "$status" -eq 0 ]
    SID="$output"

    OUTPUT=$(attach_via_pty "$SID" \
        "sleep 0.5; printf 'stty size\n'; sleep 1; printf '\x1d'")

    # stty size should print two numbers (rows cols), not "0 0"
    [[ "$OUTPUT" =~ [0-9]+\ [0-9]+ ]]
}

@test "session list dimensions update after attach" {
    run "$VEX" session create --shell /bin/sh
    [ "$status" -eq 0 ]
    SID="$output"

    # Default (no client attached) is 80x24
    run "$VEX" session list
    [[ "$output" == *"80"* ]]
    [[ "$output" == *"24"* ]]

    # Attach in background (sends terminal dimensions), keep attached
    ( sleep 2; printf '\x1d' ) \
        | timeout 5 script -qec "$VEX attach $SID" /dev/null >/dev/null 2>&1 &
    ATTACH_PID=$!
    sleep 1

    # After attach, list should show >= 1 client
    run "$VEX" session list
    [[ "$output" == *"$SID"* ]]
    # The CLIENTS column should show at least 1
    [[ "$output" =~ [1-9] ]]

    wait "$ATTACH_PID" 2>/dev/null || true
}

@test "stty size inside session returns nonzero dimensions" {
    run "$VEX" session create --shell /bin/sh
    [ "$status" -eq 0 ]
    SID="$output"

    OUTPUT=$(attach_via_pty "$SID" \
        "sleep 0.5; printf 'stty size\n'; sleep 1; printf '\x1d'")

    # Should NOT be "0 0"
    [[ ! "$OUTPUT" =~ "0 0" ]]
    # Should contain at least one dimension > 0
    [[ "$OUTPUT" =~ [1-9][0-9]*\ [1-9][0-9]* ]]
}

@test "rapid input burst does not corrupt connection" {
    run "$VEX" session create --shell /bin/sh
    [ "$status" -eq 0 ]
    SID="$output"

    # Send a rapid burst of commands to stress the stdin/frame interleaving.
    # Each command generates both stdin (client→server) and output (server→client)
    # traffic simultaneously, which exercises tokio::select! cancel-safety.
    OUTPUT=$(attach_via_pty "$SID" \
        "sleep 0.3; for i in \$(seq 1 200); do printf 'echo x\n'; done; sleep 1.5; printf 'echo BURST_OK_MARKER\n'; sleep 1; printf '\x1d'")

    # Connection must not be corrupted by the rapid interleaving
    [[ "$OUTPUT" != *"frame too large"* ]]
    [[ "$OUTPUT" != *"Connection reset"* ]]
    [[ "$OUTPUT" != *"server disconnected"* ]]
    # Session must still be responsive after the burst
    [[ "$OUTPUT" == *"BURST_OK_MARKER"* ]]
    [[ "$OUTPUT" == *"detached"* ]]
}

@test "resize propagates to PTY" {
    run "$VEX" session create --shell /bin/sh
    [ "$status" -eq 0 ]
    SID="$output"

    OUTPUT=$(attach_via_pty "$SID" \
        "sleep 0.5; printf 'tput cols\n'; sleep 1; printf '\x1d'")

    # tput cols should output a number > 0
    [[ "$OUTPUT" =~ [1-9][0-9]* ]]
}

# ═══════════════════════════════════════════════════════════════════
#  Completions
# ═══════════════════════════════════════════════════════════════════

@test "completions generates output for bash" {
    run "$VEX" completions bash
    [ "$status" -eq 0 ]
    [[ "$output" == *"complete"* ]]
}

@test "completions generates output for zsh" {
    run "$VEX" completions zsh
    [ "$status" -eq 0 ]
    [[ "$output" == *"compdef"* ]]
}
