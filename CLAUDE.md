# vex

Parallel workstream manager using git worktrees and tmux.

## Build & Test

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
```

Nix dev shell: `nix develop` (or automatic via direnv).

## Architecture

Single-binary Rust CLI. Modules in `src/`:

- `cli.rs` — clap derive definitions
- `config.rs` — `~/.vex/config.yml` management, `VEX_HOME` override
- `repo.rs` — per-repo metadata at `~/.vex/repos/<name>.yml`
- `git.rs` — shells out to `git` for worktree/branch operations
- `github.rs` — shells out to `gh` for PR lookups
- `tmux.rs` — session/window management, naming: `vex/repo/branch`
- `workstream.rs` — orchestrates git + tmux + github lifecycle
- `error.rs` — `VexError` enum with thiserror

Integration tests in `tests/integration.rs` use temp git repos with `VEX_HOME` override.

## Conventions

- Rust 2024 edition
- `cargo fmt` before committing
- `cargo clippy -- -D warnings` must pass
- Conventional commits (feat:, fix:, docs:, chore:, etc.)
- Squash merge PRs
- Shell out to external tools (git, tmux, gh) rather than using library bindings
