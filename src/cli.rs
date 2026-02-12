use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "vex", about = "Parallel workstream manager", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Create a new workstream (branch + worktree + tmux session)
    ///
    /// Auto-registers the repo with vex if not already registered.
    New {
        /// Branch name
        branch: String,

        /// Repository name (defaults to current repo)
        #[arg(short, long)]
        repo: Option<String>,
    },

    /// Switch to an existing workstream's tmux session (opens fzf picker if no branch given)
    #[command(alias = "attach")]
    Switch {
        /// Branch name of the workstream (interactive picker if omitted)
        branch: Option<String>,

        /// Repository name (defaults to current repo)
        #[arg(short, long)]
        repo: Option<String>,
    },

    /// Remove a workstream (kills tmux session, removes worktree)
    Rm {
        /// Branch name of the workstream
        branch: String,

        /// Repository name (defaults to current repo)
        #[arg(short, long)]
        repo: Option<String>,
    },

    /// List workstreams and repos
    List {
        /// Repository name (defaults to all repos)
        #[arg(short, long)]
        repo: Option<String>,
    },

    /// Open the TUI dashboard
    Open,

    /// Detach from the current tmux session
    Exit,

    /// Print the main repo path for the current workstream (use with `cd $(vex rth)`)
    #[command(alias = "return-to-home")]
    Rth {
        /// Branch name (auto-detected from cwd if omitted)
        branch: Option<String>,
        #[arg(short, long)]
        repo: Option<String>,
    },

    /// Show status of current or specified workstream
    Status {
        /// Branch name (auto-detected from cwd if omitted)
        branch: Option<String>,
        #[arg(short, long)]
        repo: Option<String>,
    },

    /// Rename a workstream (branch, worktree, tmux session, metadata)
    ///
    /// Inside a worktree: `vex rename <new-branch>` (old branch auto-detected from cwd)
    /// Outside a worktree: `vex rename <old-branch> <new-branch> -r <repo>`
    Rename {
        /// New branch name (1 arg) or old branch name (2 args)
        branch: String,

        /// New branch name when two args are given
        new_branch: Option<String>,

        /// Repository name (defaults to current repo)
        #[arg(short, long)]
        repo: Option<String>,
    },

    /// Check environment health (required tools, config, repo setup)
    Doctor,

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: ShellChoice,
    },
}

#[derive(Clone, ValueEnum)]
pub enum ShellChoice {
    Bash,
    Zsh,
    Fish,
}
