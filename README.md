# vex

Parallel workstream manager. Create isolated workstreams (git worktree + tmux session) for each feature branch you're working on.

## Install

```bash
# From crates.io
cargo install vex-cli

# With Nix (run directly)
nix run github:sandipndev/vex

# With Nix (in a flake)
# inputs.vex.url = "github:sandipndev/vex";
# then add: vex.packages.${system}.default
# or use the overlay: overlays = [vex.overlays.default]; -> pkgs.vex

# From source
cargo install --path .

# From GitHub releases
# Download binary from https://github.com/sandipndev/vex/releases
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
- `on_create` hooks run once when a workstream is created (default: `direnv allow`)

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
  on_create:
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
| `vex status [branch]` | Show status of current or specified workstream |
| `vex pr [branch]` | Create or view a GitHub PR for a workstream |
| `vex sync [-r repo]` | Sync PR metadata for workstreams |
| `vex open` | Fuzzy-pick a workstream to attach to (requires fzf) |
| `vex config` | Edit config |
| `vex reload` | Reload and validate config |

Use `-r <repo>` with `new`, `attach`, `rm`, `list` to target a repo from anywhere.

## Tmux Plugin

Vex includes a [TPM](https://github.com/tmux-plugins/tpm) plugin that binds `Prefix + V` to open the dashboard in a tmux floating popup (requires tmux 3.2+).

### Install via TPM

Add to your `~/.tmux.conf`:

```bash
set -g @plugin 'sandipndev/vex'
```

Then press `Prefix + I` to install.

### Configuration

| tmux option | Default | Description |
|---|---|---|
| `@vex-key` | `V` | Key after prefix to open popup |
| `@vex-popup-width` | `80%` | Popup width |
| `@vex-popup-height` | `80%` | Popup height |

Example overrides in `~/.tmux.conf`:

```bash
set -g @vex-key 'V'
set -g @vex-popup-width '60%'
set -g @vex-popup-height '60%'
```

## Environment

- `VEX_HOME` — override config/worktree root (default: `~/.vex`)
