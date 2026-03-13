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

    # Create a mock "claude" binary that just sleeps
    MOCK_DIR="$TEST_TMPDIR/bin"
    mkdir -p "$MOCK_DIR"
    cat > "$MOCK_DIR/claude" <<'MOCK'
#!/bin/sh
sleep 300
MOCK
    chmod +x "$MOCK_DIR/claude"

    "$VEX" daemon start 2>/dev/null

    # Verify daemon is reachable
    local i
    for i in $(seq 1 50); do
        "$VEX" list >/dev/null 2>&1 && return 0
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

vex() { "$VEX" "$@" 2>&1; }

# Attach to a session inside a PTY (via script(1)), feeding stdin from a
# subshell.
attach_via_pty() {
    local sid="$1"; shift
    ( eval "$@" ) | timeout 5 script -qec "$VEX attach $sid" /dev/null 2>&1 || true
}

# ── tests ────────────────────────────────────────────────────────────

@test "vex agent with no claude shows no sessions" {
    run vex agent
    [ "$status" -eq 0 ]
    [[ "$output" == *"no claude sessions detected"* ]]
}

@test "vex agent with session but no claude shows no sessions" {
    vex create
    run vex agent
    [ "$status" -eq 0 ]
    [[ "$output" == *"no claude sessions detected"* ]]
}

@test "vex agent detects claude process inside session" {
    id=$(vex create)

    # Launch mock claude inside the vex session
    attach_via_pty "$id" \
        "echo 'export PATH=$MOCK_DIR:\$PATH; claude &'; sleep 2"

    # Give it a moment to spawn
    sleep 1

    run vex agent
    [ "$status" -eq 0 ]
    [[ "$output" == *"$id"* ]] || {
        echo "expected session id in output, got: $output" >&2
        false
    }
    [[ "$output" == *"PID"* ]]
    [[ "$output" == *"SESSION"* ]]

    # Cleanup
    vex kill "$id"
}
