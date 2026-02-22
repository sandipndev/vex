#!/usr/bin/env bats
# Authentication tests against a Docker-containerised vexd daemon.
#
# Each test starts its own container for clean isolation — the vex client
# runs on the host while vexd runs inside the container, communicating
# only over TCP+TLS (no Unix socket shortcut).
#
# Requires: bats-core >= 1.9, docker
# Run:  bats tests/auth.bats

REPO_ROOT="$(cd "$(dirname "$BATS_TEST_FILENAME")/.." && pwd)"
VEX_BIN="$REPO_ROOT/target/debug/vex"
IMAGE_NAME="vex-auth-test"
CONTAINER_PREFIX="vex-auth"

# ── Build once before all tests in this file ─────────────────────────────────

setup_file() {
    # Build local client binary
    cargo build --manifest-path "$REPO_ROOT/Cargo.toml" --workspace 2>&3
    # Build Docker image for vexd
    docker build -t "$IMAGE_NAME" "$REPO_ROOT" 2>&3
}

teardown_file() {
    # Mop up any leftover containers from failed tests
    docker ps -aq --filter "name=${CONTAINER_PREFIX}" \
        | xargs -r docker rm -f 2>/dev/null || true
}

# ── Per-test isolation ────────────────────────────────────────────────────────
# Each test gets its own Docker container (unique TCP port) and an isolated
# HOME directory so the vex client config never leaks between tests.

setup() {
    TEST_HOME=$(mktemp -d)
    export TEST_HOME

    TCP_PORT=$((8500 + BATS_TEST_NUMBER))
    export TCP_PORT

    CONTAINER_NAME="${CONTAINER_PREFIX}-${BATS_TEST_NUMBER}"
    export CONTAINER_NAME

    # Start vexd in a fresh container
    docker run -d \
        --name "$CONTAINER_NAME" \
        -p "$TCP_PORT:7422" \
        "$IMAGE_NAME" >/dev/null

    # Wait up to 10 s for the TCP port to accept connections
    local i=0
    while (( i < 100 )); do
        if (: > /dev/tcp/127.0.0.1/$TCP_PORT) 2>/dev/null; then
            return 0
        fi
        sleep 0.1
        (( i++ )) || true
    done
    echo "Container TCP port not ready within 10 s" >&2
    docker logs "$CONTAINER_NAME" >&2
    return 1
}

teardown() {
    if [[ -n "${CONTAINER_NAME:-}" ]]; then
        docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
    fi
    rm -rf "${TEST_HOME:-}"
}

# ── Helpers ───────────────────────────────────────────────────────────────────

# Run a vex client command using the isolated config
vex()  { HOME="$TEST_HOME" "$VEX_BIN" "$@"; }

# Pipe $1 as stdin into a vex command (for 'vex connect --host' prompts)
vex_pipe() {
    local input="$1"; shift
    echo "$input" | HOME="$TEST_HOME" "$VEX_BIN" "$@"
}

# Run a vexd subcommand inside the container
vexd_exec() { docker exec "$CONTAINER_NAME" vexd "$@"; }

# Extract the pairing string from 'vexd pair' output inside the container.
# Prints "tok_<hex>:<hex>".
pair_token() {
    vexd_exec pair "$@" | grep -oE 'tok_[a-f0-9]+:[a-f0-9]+'
}

# 64 hex zeros — usable as a fake/wrong token secret
ZERO_SECRET="0000000000000000000000000000000000000000000000000000000000000000"

# ── TCP — valid token ─────────────────────────────────────────────────────────

@test "auth: valid pairing token grants access" {
    local pairing
    pairing=$(pair_token)
    [[ -n "$pairing" ]]

    vex_pipe "$pairing" connect --host "localhost:$TCP_PORT"

    run vex status
    [ "$status" -eq 0 ]
    [[ "$output" == *"vexd v"* ]]
}

@test "auth: whoami reflects the authenticated token ID" {
    local pairing tok_id
    pairing=$(pair_token)
    tok_id="${pairing%%:*}"

    vex_pipe "$pairing" connect --host "localhost:$TCP_PORT"

    run vex whoami
    [ "$status" -eq 0 ]
    [[ "$output" == *"$tok_id"* ]]
}

# ── TCP — rejected tokens ─────────────────────────────────────────────────────

@test "auth: wrong secret for a real token ID is rejected" {
    local pairing tok_id
    pairing=$(pair_token)
    tok_id="${pairing%%:*}"

    run vex_pipe "${tok_id}:${ZERO_SECRET}" connect --host "localhost:$TCP_PORT"
    [ "$status" -ne 0 ]
}

@test "auth: completely fabricated token is rejected" {
    run vex_pipe "tok_000000:${ZERO_SECRET}" connect --host "localhost:$TCP_PORT"
    [ "$status" -ne 0 ]
}

@test "auth: revoked token is rejected on subsequent connections" {
    local pairing tok_id
    pairing=$(pair_token)
    tok_id="${pairing%%:*}"

    vex_pipe "$pairing" connect --host "localhost:$TCP_PORT"
    # Confirm it works before revocation
    run vex status
    [ "$status" -eq 0 ]

    vexd_exec tokens revoke "$tok_id"

    # Every new TCP connection attempt should now fail
    run vex status
    [ "$status" -ne 0 ]
}

@test "auth: expired token is rejected after the TTL elapses" {
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

@test "auth: revoking all tokens blocks every saved connection" {
    local p1 p2
    p1=$(pair_token --label client1)
    p2=$(pair_token --label client2)
    [[ -n "$p1" && -n "$p2" ]]

    vex_pipe "$p1" connect -n conn1 --host "localhost:$TCP_PORT"
    vex_pipe "$p2" connect -n conn2 --host "localhost:$TCP_PORT"

    # Both work before revocation
    run vex status -c conn1; [ "$status" -eq 0 ]
    run vex status -c conn2; [ "$status" -eq 0 ]

    vexd_exec tokens revoke --all

    # Both fail after revocation
    run vex status -c conn1; [ "$status" -ne 0 ]
    run vex status -c conn2; [ "$status" -ne 0 ]
}

# ── TOFU TLS fingerprint pinning ──────────────────────────────────────────────

@test "auth/tofu: fingerprint is saved to config on first TCP connect" {
    local pairing
    pairing=$(pair_token)
    vex_pipe "$pairing" connect --host "localhost:$TCP_PORT"

    grep -q 'tls_fingerprint' "$TEST_HOME/.vex/config.toml"
}

@test "auth/tofu: subsequent connections with the same cert succeed" {
    local pairing
    pairing=$(pair_token)
    vex_pipe "$pairing" connect --host "localhost:$TCP_PORT"

    # First call verifies against the saved fingerprint
    run vex status
    [ "$status" -eq 0 ]

    # Second call reconnects and verifies again
    run vex status
    [ "$status" -eq 0 ]
}

@test "auth/tofu: tampered fingerprint is rejected with a clear error" {
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
