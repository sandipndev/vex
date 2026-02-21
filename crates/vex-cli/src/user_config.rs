use serde::Deserialize;
use std::path::Path;

/// Top-level user configuration loaded from `$VEX_HOME/config.yaml`.
/// All fields are optional; missing fields use defaults. If the file does
/// not exist no error is shown and defaults are used throughout.
#[derive(Debug, Default, Deserialize)]
pub struct UserConfig {
    #[serde(default)]
    pub repo: RepoConfig,
    #[serde(default)]
    pub agent: AgentConfig,
}

#[derive(Debug, Default, Deserialize)]
pub struct RepoConfig {
    #[serde(default)]
    pub register: RepoRegisterConfig,
}

#[derive(Debug, Default, Deserialize)]
pub struct RepoRegisterConfig {
    #[serde(default)]
    pub hooks: Vec<Hook>,
}

/// A shell command run in the worktree directory after `git worktree add`.
#[derive(Debug, Clone, Deserialize)]
pub struct Hook {
    pub run: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct AgentConfig {
    /// Full command string used to spawn an agent. The prompt is appended
    /// as the final argument. Defaults to `claude --dangerously-skip-permissions`.
    pub command: Option<String>,
}

impl UserConfig {
    pub fn load(vex_home: &Path) -> Self {
        let path = vex_home.join("config.yaml");
        if !path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_yaml::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn agent_command(&self) -> &str {
        self.agent
            .command
            .as_deref()
            .unwrap_or("claude --dangerously-skip-permissions")
    }

    pub fn register_hooks(&self) -> &[Hook] {
        &self.repo.register.hooks
    }
}
