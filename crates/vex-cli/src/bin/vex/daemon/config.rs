use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

const DEFAULT_AGENT_COMMAND: &str = "claude --dangerously-skip-permissions";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VexConfig {
    #[serde(default = "default_agent_command")]
    pub default_agent_command: String,
    #[serde(default)]
    pub repos: HashMap<String, RepoConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RepoConfig {
    pub agent_command: Option<String>,
}

fn default_agent_command() -> String {
    DEFAULT_AGENT_COMMAND.to_string()
}

impl VexConfig {
    pub fn load(vex_dir: &Path) -> Self {
        let path = vex_dir.join("config.yml");
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|data| serde_yaml::from_str(&data).ok())
            .unwrap_or_default()
    }

    /// Get the agent command for a repo, falling back to the global default.
    pub fn agent_command_for(&self, repo_name: &str) -> Vec<String> {
        let cmd_str = self
            .repos
            .get(repo_name)
            .and_then(|r| r.agent_command.as_deref())
            .unwrap_or(&self.default_agent_command);
        shell_split(cmd_str)
    }

}

/// Split a command string into program + args, respecting simple quoting.
fn shell_split(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;

    for ch in s.chars() {
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            ' ' | '\t' if !in_single && !in_double => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            _ => {
                current.push(ch);
            }
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}
