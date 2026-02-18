use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum VexError {
    #[error("Not inside a git repository")]
    NotAGitRepo,

    #[error("Repository '{0}' is not registered with vex. Run `vex init` first.")]
    RepoNotInitialized(String),

    #[error("Workstream '{branch}' already exists for repo '{repo}'")]
    WorkstreamAlreadyExists { repo: String, branch: String },

    #[error("Workstream '{branch}' not found for repo '{repo}'")]
    WorkstreamNotFound { repo: String, branch: String },

    #[error("Git command failed: {0}")]
    GitError(String),

    #[error("tmux command failed: {0}")]
    TmuxError(String),

    #[error("Config error: {0}")]
    ConfigError(String),

    #[error("IO error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("YAML parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("Cache error: {0}")]
    Cache(String),

    #[error("Could not determine home directory")]
    NoHomeDir,
}

impl VexError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
