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
                on_enter: vec!["direnv allow".into()],
            },
        }
    }
}

impl Config {
    pub fn load_or_create() -> Result<Self, VexError> {
        let path = Self::path()?;
        if path.exists() {
            let contents = fs::read_to_string(&path).map_err(|e| VexError::io(&path, e))?;
            let config: Config = serde_yaml::from_str(&contents)?;
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
