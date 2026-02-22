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
    local sock="$TEST_HOME/.vex/daemon/vexd.sock"
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

# Pipe $1 as stdin into a vexd admin command (for interactive prompts)
# Usage: vexd_input "<answer>" repo register <path>
vexd_input() {
    local input="$1"; shift
    printf '%s\n' "$input" | HOME="$TEST_HOME" "$VEXD_BIN" "$@"
}

# Create a minimal git repository with a single empty commit.
# Usage: make_git_repo <dir> [branch]
make_git_repo() {
    local dir="$1"
    local branch="${2:-main}"
    git -C "$dir" init -b "$branch" >/dev/null 2>&1
    git -C "$dir" -c user.email="t@t.com" -c user.name="T" \
        commit --allow-empty -m "init" >/dev/null 2>&1
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

# ── Repo management ───────────────────────────────────────────────────────────

@test "repo: register appears in repo list" {
    local repo_dir
    repo_dir=$(mktemp -d)
    make_git_repo "$repo_dir" main

    vex connect
    vexd_input "main" repo register "$repo_dir"

    run vex repo list
    [ "$status" -eq 0 ]
    [[ "$output" == *"$(basename "$repo_dir")"* ]]
    rm -rf "$repo_dir"
}

@test "repo: register auto-detects current branch as default when user presses enter" {
    local repo_dir
    repo_dir=$(mktemp -d)
    make_git_repo "$repo_dir" develop

    vex connect
    # Empty input → accept the auto-detected branch
    vexd_input "" repo register "$repo_dir"

    run vex repo list
    [ "$status" -eq 0 ]
    [[ "$output" == *"develop"* ]]
    rm -rf "$repo_dir"
}

@test "repo: custom default branch overrides the auto-detected one" {
    local repo_dir
    repo_dir=$(mktemp -d)
    make_git_repo "$repo_dir" main

    vex connect
    vexd repo register --branch release "$repo_dir"

    run vex repo list
    [ "$status" -eq 0 ]
    [[ "$output" == *"release"* ]]
    rm -rf "$repo_dir"
}

# ── Workstream management ─────────────────────────────────────────────────────

@test "workstream: create with explicit branch appears in list" {
    command -v tmux >/dev/null 2>&1 || skip "tmux not available"

    local repo_dir
    repo_dir=$(mktemp -d)
    make_git_repo "$repo_dir" main
    git -C "$repo_dir" checkout -b feature/foo >/dev/null 2>&1
    git -C "$repo_dir" checkout main >/dev/null 2>&1

    vex connect
    vexd_input "main" repo register "$repo_dir"

    local repo_id
    repo_id=$(vex repo list | awk 'NR==2 {print $1}')

    run vex workstream create "$repo_id" --branch feature/foo
    [ "$status" -eq 0 ]
    [[ "$output" == *"feature/foo"* ]]

    run vex workstream list
    [ "$status" -eq 0 ]
    [[ "$output" == *"feature/foo"* ]]
    rm -rf "$repo_dir"
}

@test "workstream: create without --branch uses the repo default branch" {
    command -v tmux >/dev/null 2>&1 || skip "tmux not available"

    local repo_dir
    repo_dir=$(mktemp -d)
    make_git_repo "$repo_dir" develop
    # Switch away from develop so it is not checked out (worktrees cannot
    # be created on the currently-active branch).
    git -C "$repo_dir" checkout -b _parked >/dev/null 2>&1

    vex connect
    # Explicitly set "develop" as the default branch at registration time
    vexd repo register --branch develop "$repo_dir"

    local repo_id
    repo_id=$(vex repo list | awk 'NR==2 {print $1}')

    run vex workstream create "$repo_id"
    [ "$status" -eq 0 ]
    [[ "$output" == *"develop"* ]]
    rm -rf "$repo_dir"
}
