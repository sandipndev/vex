#!/usr/bin/env bats
# End-to-end tests for local daemon behaviour (Unix socket + multi-connection).
# Authentication tests live in auth.bats (Docker-based).
#
# Requires: bats-core >= 1.9
# Run:  bats --jobs 4 tests/e2e.bats
# Or:   cargo build --workspace && bats --jobs 4 tests/e2e.bats

REPO_ROOT="$(cd "$(dirname "$BATS_TEST_FILENAME")/.." && pwd)"
VEXD_BIN="$REPO_ROOT/target/debug/vexd"
VEX_BIN="$REPO_ROOT/target/debug/vex"

# ── Build once before all tests in this file ─────────────────────────────────

setup_file() {
    cargo build --manifest-path "$REPO_ROOT/Cargo.toml" --workspace 2>&3
}

# ── Per-test isolation ────────────────────────────────────────────────────────
# Each test gets its own HOME directory and TCP port so that all tests can run
# in parallel without port conflicts or shared state.

setup() {
    TEST_HOME=$(mktemp -d)
    export TEST_HOME

    # Assign a unique TCP port per test: 7500, 7501, 7502, …
    TCP_PORT=$((7500 + BATS_TEST_NUMBER))
    export TCP_PORT
    export VEXD_TCP_PORT="$TCP_PORT"

    # Start daemon in the background using the isolated home and port
    HOME="$TEST_HOME" "$VEXD_BIN" start &
    VEXD_PID=$!
    export VEXD_PID

    # Wait up to 5 s for the Unix socket AND the TCP port to be ready
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

# ── Helpers ───────────────────────────────────────────────────────────────────

# Run a vexd admin command against the isolated daemon
vexd() { HOME="$TEST_HOME" "$VEXD_BIN" "$@"; }

# Run a vex client command using the isolated config
vex()  { HOME="$TEST_HOME" "$VEX_BIN"  "$@"; }

# Pipe $1 as stdin into a vex command (for 'vex connect --host' prompts)
# Usage: vex_pipe "<pairing-string>" connect [flags...]
vex_pipe() {
    local input="$1"; shift
    echo "$input" | HOME="$TEST_HOME" "$VEX_BIN" "$@"
}

# Extract the pairing string from 'vexd pair' output.
# Prints "tok_<hex>:<hex>" — the only token in that format in the output.
pair_token() {
    vexd pair "$@" | grep -oE 'tok_[a-f0-9]+:[a-f0-9]+'
}

# 64 hex zeros — usable as a fake/wrong token secret
ZERO_SECRET="0000000000000000000000000000000000000000000000000000000000000000"

# ── Unix socket (no auth required) ───────────────────────────────────────────

@test "unix: status succeeds without any authentication" {
    vex connect

    run vex status
    [ "$status" -eq 0 ]
    [[ "$output" == *"vexd v"* ]]
}

@test "unix: whoami reports local admin identity" {
    vex connect

    run vex whoami
    [ "$status" -eq 0 ]
    [[ "$output" == *"local"* ]]
}

# ── Multi-connection — mixed transports ───────────────────────────────────────

@test "multi: unix and tcp connections can coexist and both work" {
    # Local connection saved as 'local'
    vex connect -n local

    # Remote TCP connection saved as 'remote'
    local pairing
    pairing=$(pair_token)
    vex_pipe "$pairing" connect -n remote --host "localhost:$TCP_PORT"

    run vex status -c local
    [ "$status" -eq 0 ]
    [[ "$output" == *"vexd v"* ]]

    run vex status -c remote
    [ "$status" -eq 0 ]
    [[ "$output" == *"vexd v"* ]]
}

@test "multi: --all queries every saved connection" {
    vex connect -n local

    local pairing
    pairing=$(pair_token)
    vex_pipe "$pairing" connect -n remote --host "localhost:$TCP_PORT"

    run vex status --all
    [ "$status" -eq 0 ]
    # Output should contain results labelled for both connections
    [[ "$output" == *"[local]"* ]]
    [[ "$output" == *"[remote]"* ]]
}
