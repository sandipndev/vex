use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "vex", about = "Parallel workstream manager", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Register the current git repository with vex
    Init,

    /// Create a new workstream (branch + worktree + tmux session)
    ///
    /// Use "#123" as the branch to open a GitHub PR by number.
    New {
        /// Branch name or "#<pr-number>" to reference a GitHub PR
        branch: String,

        /// Repository name (defaults to current repo)
        #[arg(short, long)]
        repo: Option<String>,
    },

    /// Attach to an existing workstream's tmux session
    Attach {
        /// Branch name of the workstream
        branch: String,

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

    /// List workstreams
    List {
        /// Repository name (defaults to all repos)
        #[arg(short, long)]
        repo: Option<String>,
    },

    /// List registered repositories
    Repos,

    /// Open vex config in $EDITOR
    Config,
}
