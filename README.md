# vex

Parallel workstream manager. Create isolated workstreams (git worktree + tmux session) for each feature branch you're working on.

## Install

```bash
# From source (requires Rust toolchain)
cargo install --path .

# With Nix
nix build
```

## Quick Start

```bash
# Register a repo
cd ~/projects/my-app
vex init

# Start a new workstream — creates worktree + tmux session
vex new feature-auth

# Or open a GitHub PR directly
vex new '#42'

# List workstreams
vex list

# Re-attach to a workstream
vex attach feature-auth

# Clean up
vex rm feature-auth
```

## How It Works

Each workstream is a **git worktree** + **tmux session**:

- Worktrees live at `~/.vex/worktrees/<repo>/<branch>/`
- tmux sessions are named `vex/<repo>/<branch>`
- Default windows: `nvim`, `claude`, `zsh`
- Hooks run on enter (default: `direnv allow`)

When you run `vex new <branch>`:
1. Fetches from origin
2. If the branch exists remotely, tracks it; otherwise creates it off the default branch
3. Creates a git worktree
4. Opens a tmux session with your configured windows

## Configuration

```bash
vex config    # opens ~/.vex/config.yml in $EDITOR
vex reload    # validate and show current config
```

Default `~/.vex/config.yml`:

```yaml
windows:
  - name: nvim
    command: nvim
  - name: claude
    command: claude
  - name: zsh
    command: ''
hooks:
  on_enter:
    - direnv allow
```

## Commands

| Command | Description |
|---|---|
| `vex init` | Register current git repo |
| `vex new <branch>` | Create workstream (worktree + tmux session) |
| `vex new '#<number>'` | Create workstream from a GitHub PR |
| `vex attach <branch>` | Attach to existing workstream |
| `vex rm <branch>` | Remove workstream |
| `vex list [-r repo]` | List workstreams |
| `vex repos` | List registered repositories |
| `vex config` | Edit config |
| `vex reload` | Reload and validate config |

Use `-r <repo>` with `new`, `attach`, `rm`, `list` to target a repo from anywhere.

## Environment

- `VEX_HOME` — override config/worktree root (default: `~/.vex`)
