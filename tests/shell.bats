#!/usr/bin/env bats
# Shell management tests — tmux backend, local daemon + TCP access control.
#
# Requires: bats-core >= 1.9, tmux
# Run:  bats tests/shell.bats

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

    TCP_PORT=$((7800 + BATS_TEST_NUMBER * 2))
    HTTP_PORT=$((TCP_PORT + 1))
    export TCP_PORT HTTP_PORT
    export VEXD_TCP_PORT="$TCP_PORT"

    HOME="$TEST_HOME" "$VEXD_BIN" start &
    VEXD_PID=$!
    export VEXD_PID

    local sock="$TEST_HOME/.vex/vexd.sock"
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
    # Kill any vex_* tmux sessions to prevent test pollution
    tmux list-sessions -F '#{session_name}' 2>/dev/null \
        | grep '^vex_' \
        | while read -r sess; do
            tmux kill-session -t "$sess" 2>/dev/null || true
        done

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

register_project() {
    local tmpdir
    tmpdir=$(mktemp -d)
    vexd project register "$1" "owner/$1" "$tmpdir"
    echo "$tmpdir"
}

# ── Tests ─────────────────────────────────────────────────────────────────

@test "shell: create succeeds" {
    register_project myproject
    vex connect
    vex workstream create myproject ws1

    run vex shell create myproject ws1
    [ "$status" -eq 0 ]
    [[ "$output" == *"Created"* ]]
    [[ "$output" == *"shell_1"* ]]
}

@test "shell: list shows created shells" {
    register_project myproject
    vex connect
    vex workstream create myproject ws1
    vex shell create myproject ws1
    vex shell create myproject ws1

    run vex shell list myproject ws1
    [ "$status" -eq 0 ]
    [[ "$output" == *"shell_1"* ]]
    [[ "$output" == *"shell_2"* ]]
}

@test "shell: delete removes shell" {
    register_project myproject
    vex connect
    vex workstream create myproject ws1
    vex shell create myproject ws1

    run vex shell delete myproject ws1 shell_1
    [ "$status" -eq 0 ]
    [[ "$output" == *"Deleted"* ]]

    run vex shell list myproject ws1
    [ "$status" -eq 0 ]
    [[ "$output" == *"No shells"* ]]
}

@test "shell: create fails for non-existent workstream" {
    register_project myproject
    vex connect

    run vex shell create myproject nosuchws
    [ "$status" -ne 0 ]
    [[ "$output" == *"not found"* ]]
}

@test "shell: delete fails for non-existent shell" {
    register_project myproject
    vex connect
    vex workstream create myproject ws1

    run vex shell delete myproject ws1 shell_99
    [ "$status" -ne 0 ]
    [[ "$output" == *"NotFound"* ]]
}

@test "shell: tmux session created on first shell" {
    register_project myproject
    vex connect
    vex workstream create myproject ws1
    vex shell create myproject ws1

    run tmux has-session -t vex_myproject_ws1
    [ "$status" -eq 0 ]
}

@test "shell: tmux session destroyed on last shell delete" {
    register_project myproject
    vex connect
    vex workstream create myproject ws1
    vex shell create myproject ws1

    vex shell delete myproject ws1 shell_1

    run tmux has-session -t vex_myproject_ws1
    [ "$status" -ne 0 ]
}

@test "shell: workstream delete kills all shells and tmux session" {
    register_project myproject
    vex connect
    vex workstream create myproject ws1
    vex shell create myproject ws1
    vex shell create myproject ws1

    run tmux has-session -t vex_myproject_ws1
    [ "$status" -eq 0 ]

    vex workstream delete myproject ws1

    run tmux has-session -t vex_myproject_ws1
    [ "$status" -ne 0 ]
}

@test "shell: bidirectional sync removes vanished tmux windows" {
    register_project myproject
    vex connect
    vex workstream create myproject ws1
    vex shell create myproject ws1

    # Kill the tmux session directly (simulating user closing it)
    tmux kill-session -t vex_myproject_ws1

    # Wait for the monitor to reconcile (polls every 3s)
    sleep 4

    run vex shell list myproject ws1
    [ "$status" -eq 0 ]
    [[ "$output" == *"No shells"* ]]
}

@test "shell: list works over TCP; create/delete rejected over TCP" {
    register_project myproject
    vex connect
    vex workstream create myproject ws1

    local pairing
    pairing=$(pair_token)
    vex_pipe "$pairing" connect -n remote --host "localhost:$TCP_PORT"

    # Create a shell via local socket first
    vex shell create myproject ws1

    # List should work over TCP
    run vex shell list myproject ws1 -c remote
    [ "$status" -eq 0 ]
    [[ "$output" == *"shell_1"* ]]

    # Create should be rejected over TCP
    run vex shell create myproject ws1 -c remote
    [ "$status" -ne 0 ]
    [[ "$output" == *"LocalOnly"* ]]

    # Delete should be rejected over TCP
    run vex shell delete myproject ws1 shell_1 -c remote
    [ "$status" -ne 0 ]
    [[ "$output" == *"LocalOnly"* ]]
}
