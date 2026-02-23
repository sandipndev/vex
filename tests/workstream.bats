#!/usr/bin/env bats
# Workstream CRUD tests — local daemon (Unix socket) + TCP.
#
# Requires: bats-core >= 1.9
# Run:  bats tests/workstream.bats

REPO_ROOT="$(cd "$(dirname "$BATS_TEST_FILENAME")/.." && pwd)"
VEXD_BIN="$REPO_ROOT/target/debug/vexd"
VEX_BIN="$REPO_ROOT/target/debug/vex"

# ── Build once before all tests in this file ─────────────────────────────

setup_file() {
    cargo build --manifest-path "$REPO_ROOT/Cargo.toml" --workspace 2>&3
}

# ── Per-test isolation ────────────────────────────────────────────────────

setup() {
    TEST_HOME=$(mktemp -d)
    export TEST_HOME

    TCP_PORT=$((7700 + BATS_TEST_NUMBER * 2))
    HTTP_PORT=$((TCP_PORT + 1))
    export TCP_PORT HTTP_PORT
    export VEXD_TCP_PORT="$TCP_PORT"

    HOME="$TEST_HOME" "$VEXD_BIN" start &
    VEXD_PID=$!
    export VEXD_PID

    local sock="$TEST_HOME/.vexd/vexd.sock"
    local i=0
    while (( i < 50 )); do
        if [[ -S "$sock" ]] \
           && (: > /dev/tcp/127.0.0.1/$TCP_PORT) 2>/dev/null; then
            return 0
        fi
        sleep 0.1
        (( i++ )) || true
    done
    echo "vexd did not start within 5 s" >&2
    return 1
}

teardown() {
    if [[ -n "${VEXD_PID:-}" ]]; then
        kill "$VEXD_PID" 2>/dev/null || true
        wait "$VEXD_PID" 2>/dev/null || true
    fi
    rm -rf "${TEST_HOME:-}"
}

# ── Helpers ───────────────────────────────────────────────────────────────

vexd() { HOME="$TEST_HOME" "$VEXD_BIN" "$@"; }
vex()  { HOME="$TEST_HOME" "$VEX_BIN"  "$@"; }

vex_pipe() {
    local input="$1"; shift
    echo "$input" | HOME="$TEST_HOME" "$VEX_BIN" "$@"
}

pair_token() {
    vexd pair "$@" | grep -oE 'tok_[a-f0-9]+:[a-f0-9]+'
}

register_repo() {
    local tmpdir
    tmpdir=$(mktemp -d)
    vexd repo register "$1" "$tmpdir"
    echo "$tmpdir"
}

# ── Tests ─────────────────────────────────────────────────────────────────

@test "workstream: create succeeds" {
    register_repo myrepo

    vex connect
    run vex workstream create myrepo ws1
    [ "$status" -eq 0 ]
    [[ "$output" == *"Created"* ]]
    [[ "$output" == *"ws1"* ]]
}

@test "workstream: list shows created workstream" {
    register_repo myrepo

    vex connect
    vex workstream create myrepo ws1
    vex workstream create myrepo ws2

    run vex workstream list myrepo
    [ "$status" -eq 0 ]
    [[ "$output" == *"ws1"* ]]
    [[ "$output" == *"ws2"* ]]
}

@test "workstream: delete removes workstream" {
    register_repo myrepo

    vex connect
    vex workstream create myrepo ws1

    run vex workstream delete myrepo ws1
    [ "$status" -eq 0 ]
    [[ "$output" == *"Deleted"* ]]

    run vex workstream list myrepo
    [ "$status" -eq 0 ]
    [[ "$output" == *"No workstreams"* ]]
}

@test "workstream: create rejects duplicate name" {
    register_repo myrepo

    vex connect
    vex workstream create myrepo ws1

    run vex workstream create myrepo ws1
    [ "$status" -ne 0 ]
    [[ "$output" == *"already exists"* ]]
}

@test "workstream: create fails for non-existent repo" {
    vex connect

    run vex workstream create nosuchrepo ws1
    [ "$status" -ne 0 ]
    [[ "$output" == *"not found"* ]]
}

@test "workstream: CRUD works over TCP" {
    register_repo myrepo

    local pairing
    pairing=$(pair_token)
    vex_pipe "$pairing" connect --host "localhost:$TCP_PORT"

    run vex workstream create myrepo ws1
    [ "$status" -eq 0 ]
    [[ "$output" == *"Created"* ]]

    run vex workstream list myrepo
    [ "$status" -eq 0 ]
    [[ "$output" == *"ws1"* ]]

    run vex workstream delete myrepo ws1
    [ "$status" -eq 0 ]
    [[ "$output" == *"Deleted"* ]]

    run vex workstream list myrepo
    [ "$status" -eq 0 ]
    [[ "$output" == *"No workstreams"* ]]
}
