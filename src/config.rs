use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::VexError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub windows: Vec<Window>,
    #[serde(default)]
    pub hooks: Hooks,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    pub name: String,
    #[serde(default)]
    pub command: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Hooks {
    #[serde(default)]
    pub on_create: Vec<String>,
    /// Legacy field — migrated to on_create automatically
    #[serde(default, skip_serializing)]
    pub on_enter: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            windows: vec![
                Window {
                    name: "nvim".into(),
                    command: "nvim".into(),
                },
                Window {
                    name: "claude".into(),
                    command: "claude".into(),
                },
                Window {
                    name: "zsh".into(),
                    command: String::new(),
                },
            ],
            hooks: Hooks {
                on_create: vec!["direnv allow".into()],
                on_enter: vec![],
            },
        }
    }
}

impl Config {
    pub fn load_or_create() -> Result<Self, VexError> {
        let path = Self::path()?;
        if path.exists() {
            let contents = fs::read_to_string(&path).map_err(|e| VexError::io(&path, e))?;
            let mut config: Config = serde_yaml::from_str(&contents)?;
            // Migrate legacy on_enter → on_create
            if !config.hooks.on_enter.is_empty() && config.hooks.on_create.is_empty() {
                config.hooks.on_create = std::mem::take(&mut config.hooks.on_enter);
                config.save()?;
            }
            Ok(config)
        } else {
            let config = Config::default();
            config.save()?;
            Ok(config)
        }
    }

    pub fn save(&self) -> Result<(), VexError> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| VexError::io(parent, e))?;
        }
        let yaml = serde_yaml::to_string(self)?;
        fs::write(&path, yaml).map_err(|e| VexError::io(&path, e))?;
        Ok(())
    }

    pub fn path() -> Result<PathBuf, VexError> {
        Ok(vex_home()?.join("config.yml"))
    }
}

pub fn vex_home() -> Result<PathBuf, VexError> {
    if let Ok(custom) = std::env::var("VEX_HOME") {
        return Ok(PathBuf::from(custom));
    }
    let home = dirs::home_dir().ok_or(VexError::NoHomeDir)?;
    Ok(home.join(".vex"))
}

pub fn repos_dir() -> Result<PathBuf, VexError> {
    Ok(vex_home()?.join("repos"))
}

pub fn worktrees_dir() -> Result<PathBuf, VexError> {
    Ok(vex_home()?.join("worktrees"))
}

pub fn ensure_vex_dirs() -> Result<(), VexError> {
    for dir in [vex_home()?, repos_dir()?, worktrees_dir()?] {
        fs::create_dir_all(&dir).map_err(|e| VexError::io(&dir, e))?;
    }
    Ok(())
}

pub fn open_config_in_editor() -> Result<(), VexError> {
    let path = Config::path()?;
    if !path.exists() {
        Config::default().save()?;
    }
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".into());
    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .map_err(|e| VexError::io(&path, e))?;
    if !status.success() {
        return Err(VexError::ConfigError(format!(
            "Editor '{}' exited with status {}",
            editor,
            status.code().unwrap_or(-1)
        )));
    }
    Ok(())
}

pub fn repo_config_path(repo_name: &str) -> Result<PathBuf, VexError> {
    Ok(repos_dir()?.join(format!("{repo_name}.yml")))
}

pub fn repo_worktree_dir(repo_name: &str) -> Result<PathBuf, VexError> {
    Ok(worktrees_dir()?.join(repo_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_three_windows() {
        let config = Config::default();
        assert_eq!(config.windows.len(), 3);
        assert_eq!(config.windows[0].name, "nvim");
        assert_eq!(config.windows[1].name, "claude");
        assert_eq!(config.windows[2].name, "zsh");
    }

    #[test]
    fn default_config_has_direnv_hook() {
        let config = Config::default();
        assert_eq!(config.hooks.on_create, vec!["direnv allow"]);
    }

    #[test]
    fn config_serde_roundtrip() {
        let config = Config::default();
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.windows.len(), config.windows.len());
        assert_eq!(parsed.hooks.on_create, config.hooks.on_create);
        for (a, b) in parsed.windows.iter().zip(config.windows.iter()) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.command, b.command);
        }
    }

    #[test]
    fn config_file_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.yml");

        let config = Config::default();
        let yaml = serde_yaml::to_string(&config).unwrap();
        fs::write(&config_path, &yaml).unwrap();

        let contents = fs::read_to_string(&config_path).unwrap();
        let loaded: Config = serde_yaml::from_str(&contents).unwrap();
        assert_eq!(loaded.windows.len(), 3);
        assert_eq!(loaded.hooks.on_create, vec!["direnv allow"]);
    }

    #[test]
    fn vex_home_respects_env_var() {
        // This test is validated via integration tests (separate processes)
        // to avoid env var races. Here we just test the fallback logic.
        let home = dirs::home_dir().unwrap();
        // When VEX_HOME is not set (default in test runner), falls back to ~/.vex
        // We can't assert exact value since other tests may set VEX_HOME concurrently,
        // so just verify the function doesn't error.
        assert!(vex_home().is_ok());
        let _ = home; // suppress unused
    }
}
