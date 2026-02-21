#!/usr/bin/env bats
# End-to-end tests for all vex/vexd authentication modes.
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

# ── TCP — valid token ─────────────────────────────────────────────────────────

@test "tcp: valid pairing token grants access" {
    local pairing
    pairing=$(pair_token)
    [[ -n "$pairing" ]]

    vex_pipe "$pairing" connect --host "localhost:$TCP_PORT"

    run vex status
    [ "$status" -eq 0 ]
    [[ "$output" == *"vexd v"* ]]
}

@test "tcp: whoami reflects the authenticated token ID" {
    local pairing tok_id
    pairing=$(pair_token)
    tok_id="${pairing%%:*}"

    vex_pipe "$pairing" connect --host "localhost:$TCP_PORT"

    run vex whoami
    [ "$status" -eq 0 ]
    [[ "$output" == *"$tok_id"* ]]
}

# ── TCP — rejected tokens ─────────────────────────────────────────────────────

@test "tcp: wrong secret for a real token ID is rejected" {
    local pairing tok_id
    pairing=$(pair_token)
    tok_id="${pairing%%:*}"

    run vex_pipe "${tok_id}:${ZERO_SECRET}" connect --host "localhost:$TCP_PORT"
    [ "$status" -ne 0 ]
}

@test "tcp: completely fabricated token is rejected" {
    run vex_pipe "tok_000000:${ZERO_SECRET}" connect --host "localhost:$TCP_PORT"
    [ "$status" -ne 0 ]
}

@test "tcp: revoked token is rejected on subsequent connections" {
    local pairing tok_id
    pairing=$(pair_token)
    tok_id="${pairing%%:*}"

    vex_pipe "$pairing" connect --host "localhost:$TCP_PORT"
    # Confirm it works before revocation
    run vex status
    [ "$status" -eq 0 ]

    vexd tokens revoke "$tok_id"

    # Every new TCP connection attempt should now fail
    run vex status
    [ "$status" -ne 0 ]
}

@test "tcp: expired token is rejected after the TTL elapses" {
    local pairing
    pairing=$(pair_token --expire 1)   # 1-second lifetime
    [[ -n "$pairing" ]]

    vex_pipe "$pairing" connect --host "localhost:$TCP_PORT"

    # Works while still within TTL
    run vex status
    [ "$status" -eq 0 ]

    sleep 2   # let the 1-second TTL expire

    # Rejected after expiry
    run vex status
    [ "$status" -ne 0 ]
}

@test "tcp: revoking all tokens blocks every saved connection" {
    local p1 p2
    p1=$(pair_token --label client1)
    p2=$(pair_token --label client2)
    [[ -n "$p1" && -n "$p2" ]]

    vex_pipe "$p1" connect -n conn1 --host "localhost:$TCP_PORT"
    vex_pipe "$p2" connect -n conn2 --host "localhost:$TCP_PORT"

    # Both work before revocation
    run vex status -c conn1; [ "$status" -eq 0 ]
    run vex status -c conn2; [ "$status" -eq 0 ]

    vexd tokens revoke --all

    # Both fail after revocation
    run vex status -c conn1; [ "$status" -ne 0 ]
    run vex status -c conn2; [ "$status" -ne 0 ]
}

# ── TOFU TLS fingerprint pinning ──────────────────────────────────────────────

@test "tofu: fingerprint is saved to config on first TCP connect" {
    local pairing
    pairing=$(pair_token)
    vex_pipe "$pairing" connect --host "localhost:$TCP_PORT"

    grep -q 'tls_fingerprint' "$TEST_HOME/.vex/config.toml"
}

@test "tofu: subsequent connections with the same cert succeed" {
    local pairing
    pairing=$(pair_token)
    vex_pipe "$pairing" connect --host "localhost:$TCP_PORT"

    # First call establishes the pinned fingerprint (already done above)
    run vex status
    [ "$status" -eq 0 ]

    # Second call reconnects and verifies against the saved fingerprint
    run vex status
    [ "$status" -eq 0 ]
}

@test "tofu: tampered fingerprint is rejected with a clear error" {
    local pairing
    pairing=$(pair_token)
    vex_pipe "$pairing" connect --host "localhost:$TCP_PORT"

    # Corrupt the saved fingerprint in-place
    local fake
    fake=$(printf 'a%.0s' {1..64})
    sed -i \
        "s/tls_fingerprint = \"[a-f0-9]*\"/tls_fingerprint = \"$fake\"/" \
        "$TEST_HOME/.vex/config.toml"

    run vex status
    [ "$status" -ne 0 ]
    # Error message should mention the mismatch
    [[ "$output" == *"fingerprint"* ]] || [[ "$output" == *"mismatch"* ]]
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

@test "multi: status queries every saved connection in parallel" {
    vex connect -n local

    local pairing
    pairing=$(pair_token)
    vex_pipe "$pairing" connect -n remote --host "localhost:$TCP_PORT"

    run vex status
    [ "$status" -eq 0 ]
    # Output should contain results labelled for both connections
    [[ "$output" == *"[local]"* ]]
    [[ "$output" == *"[remote]"* ]]
}
