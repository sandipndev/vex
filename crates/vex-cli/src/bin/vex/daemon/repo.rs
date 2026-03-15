use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, bail};
use tokio::sync::Mutex;
use vex_cli::proto::RepoEntry;

pub type RepoStore = Arc<Mutex<RepoStoreInner>>;

pub struct RepoStoreInner {
    repos: HashMap<String, PathBuf>,
    persist_path: PathBuf,
}

impl RepoStoreInner {
    pub fn load(vex_dir: &Path) -> Self {
        let persist_path = vex_dir.join("repos.json");
        let repos = std::fs::read_to_string(&persist_path)
            .ok()
            .and_then(|data| serde_json::from_str::<HashMap<String, PathBuf>>(&data).ok())
            .unwrap_or_default();
        Self {
            repos,
            persist_path,
        }
    }

    pub fn add(&mut self, name: String, path: PathBuf) -> Result<()> {
        if !path.is_dir() {
            bail!("path does not exist or is not a directory: {}", path.display());
        }
        let path = std::fs::canonicalize(&path)?;
        self.repos.insert(name, path);
        self.flush()
    }

    pub fn remove(&mut self, name: &str) -> Result<()> {
        if self.repos.remove(name).is_none() {
            bail!("repo '{}' not found", name);
        }
        self.flush()
    }

    pub fn list(&self) -> Vec<RepoEntry> {
        self.repos
            .iter()
            .map(|(name, path)| RepoEntry {
                name: name.clone(),
                path: path.clone(),
            })
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<PathBuf> {
        self.repos.get(name).cloned()
    }

    fn flush(&self) -> Result<()> {
        let data = serde_json::to_string_pretty(&self.repos)?;
        std::fs::write(&self.persist_path, data)?;
        Ok(())
    }
}

pub fn new_repo_store(vex_dir: &Path) -> RepoStore {
    Arc::new(Mutex::new(RepoStoreInner::load(vex_dir)))
}

/// Introspect a path for git repository information.
pub fn introspect_path(path: &Path) -> (String, PathBuf, Option<String>, Option<String>) {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let suggested_name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unnamed".to_string());

    let git_remote = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(&canonical)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    let git_branch = std::process::Command::new("git")
        .args(["symbolic-ref", "--short", "HEAD"])
        .current_dir(&canonical)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    (suggested_name, canonical, git_remote, git_branch)
}
