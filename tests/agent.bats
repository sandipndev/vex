#!/usr/bin/env bats

# ── file-level: build binaries once ──────────────────────────────────

setup_file() {
    cd "$BATS_TEST_DIRNAME/.."
    cargo build --quiet
}

# ── per-test: isolated vexd instance with mock claude ────────────────

setup() {
    VEX="$BATS_TEST_DIRNAME/../target/debug/vex"

    TEST_TMPDIR="$(mktemp -d)"
    export VEX_DIR="$TEST_TMPDIR/.vex"
    export VEX_PORT=$((20000 + RANDOM % 10000))

    # Prepend mock-claude dir to PATH so `claude` resolves to our mock
    export PATH="$BATS_TEST_DIRNAME/mock-claude:$PATH"

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

UUID_RE='^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'

vex() { "$VEX" "$@" 2>&1; }

# ── tests ────────────────────────────────────────────────────────────

@test "agent create returns valid UUID" {
    run vex agent create
    [ "$status" -eq 0 ]
    [[ "${lines[0]}" =~ $UUID_RE ]]
}

@test "agent create with options returns valid UUID" {
    run vex agent create --model sonnet --permission-mode plan --max-turns 5
    [ "$status" -eq 0 ]
    [[ "${lines[0]}" =~ $UUID_RE ]]
}

@test "agent list shows agent with idle status" {
    id=$(vex agent create)
    run vex agent list
    [ "$status" -eq 0 ]
    [[ "$output" == *"$id"* ]]
    [[ "$output" == *"idle"* ]]
}

@test "agent list shows no agents initially" {
    run vex agent list
    [ "$status" -eq 0 ]
    [[ "$output" == *"no agents"* ]]
}

@test "agent list alias ls works" {
    vex agent create
    run vex agent ls
    [ "$status" -eq 0 ]
    [[ "$output" == *"idle"* ]]
}

@test "agent status shows idle" {
    id=$(vex agent create)
    run vex agent status "$id"
    [ "$status" -eq 0 ]
    [[ "$output" == *"idle"* ]]
    [[ "$output" == *"Turns:      0"* ]]
}

@test "agent prompt completes and shows output" {
    id=$(vex agent create)
    run vex agent prompt "$id" "test prompt"
    [ "$status" -eq 0 ]
    [[ "$output" == *"Hello from mock"* ]]
    [[ "$output" == *"prompt done"* ]]
}

@test "agent status after prompt shows turn count 1" {
    id=$(vex agent create)
    vex agent prompt "$id" "first"
    run vex agent status "$id"
    [ "$status" -eq 0 ]
    [[ "$output" == *"Turns:      1"* ]]
    [[ "$output" == *"mock-session-123"* ]]
}

@test "follow-up prompt uses --resume with session id" {
    id=$(vex agent create)
    vex agent prompt "$id" "first prompt"
    vex agent prompt "$id" "second prompt"

    # Check that the second invocation had --resume
    run cat "$VEX_DIR/claude-args.log"
    [ "$status" -eq 0 ]
    # Second line should contain --resume mock-session-123
    [[ "${lines[1]}" == *"--resume"* ]]
    [[ "${lines[1]}" == *"mock-session-123"* ]]
}

@test "agent status after two prompts shows turn count 2" {
    id=$(vex agent create)
    vex agent prompt "$id" "first"
    vex agent prompt "$id" "second"
    run vex agent status "$id"
    [ "$status" -eq 0 ]
    [[ "$output" == *"Turns:      2"* ]]
}

@test "agent kill removes agent" {
    id=$(vex agent create)
    run vex agent kill "$id"
    [ "$status" -eq 0 ]
    [[ "$output" == *"killed agent"* ]]

    # Should be gone now
    run vex agent list
    [[ "$output" == *"no agents"* ]]
}

@test "prompt on nonexistent agent fails" {
    run vex agent prompt "00000000-0000-0000-0000-000000000000" "test"
    [ "$status" -ne 0 ]
    [[ "$output" == *"not found"* ]]
}

@test "kill nonexistent agent fails" {
    run vex agent kill "00000000-0000-0000-0000-000000000000"
    [ "$status" -ne 0 ]
    [[ "$output" == *"not found"* ]]
}

@test "status on nonexistent agent fails" {
    run vex agent status "00000000-0000-0000-0000-000000000000"
    [ "$status" -ne 0 ]
    [[ "$output" == *"not found"* ]]
}

@test "agent id prefix resolution works" {
    id=$(vex agent create)
    prefix="${id:0:8}"
    run vex agent status "$prefix"
    [ "$status" -eq 0 ]
    [[ "$output" == *"$id"* ]]
}

@test "agent create with model passes --model to claude" {
    id=$(vex agent create --model opus)
    vex agent prompt "$id" "hello"

    run cat "$VEX_DIR/claude-args.log"
    [[ "${lines[0]}" == *"--model opus"* ]]
}

@test "multiple agents can coexist" {
    id1=$(vex agent create)
    id2=$(vex agent create)
    run vex agent list
    [ "$status" -eq 0 ]
    [[ "$output" == *"$id1"* ]]
    [[ "$output" == *"$id2"* ]]
}

@test "claude stderr is surfaced on failure" {
    id=$(vex agent create)
    touch "$VEX_DIR/mock-fail"
    run vex agent prompt "$id" "test"
    [ "$status" -eq 0 ]
    [[ "$output" == *"authentication required"* ]]
}

@test "agent status shows error after failed prompt" {
    id=$(vex agent create)
    touch "$VEX_DIR/mock-fail"
    vex agent prompt "$id" "test"
    run vex agent status "$id"
    [ "$status" -eq 0 ]
    [[ "$output" == *"error:"* ]]
    [[ "$output" == *"authentication required"* ]]
}
