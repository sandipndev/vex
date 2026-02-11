use std::fs;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::{ensure_vex_dirs, repo_config_path};
use crate::error::VexError;
use crate::git;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoMetadata {
    pub name: String,
    pub path: String,
    pub default_branch: String,
    #[serde(default)]
    pub workstreams: Vec<WorkstreamEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkstreamEntry {
    pub branch: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub pr_number: Option<u64>,
}

impl RepoMetadata {
    pub fn load(repo_name: &str) -> Result<Self, VexError> {
        let path = repo_config_path(repo_name)?;
        if !path.exists() {
            return Err(VexError::RepoNotInitialized(repo_name.into()));
        }
        let contents = fs::read_to_string(&path).map_err(|e| VexError::io(&path, e))?;
        let meta: Self = serde_yaml::from_str(&contents)?;
        Ok(meta)
    }

    pub fn save(&self) -> Result<(), VexError> {
        let path = repo_config_path(&self.name)?;
        let yaml = serde_yaml::to_string(self)?;
        fs::write(&path, yaml).map_err(|e| VexError::io(&path, e))?;
        Ok(())
    }

    pub fn add_workstream(&mut self, branch: &str, pr_number: Option<u64>) {
        if !self.workstreams.iter().any(|w| w.branch == branch) {
            self.workstreams.push(WorkstreamEntry {
                branch: branch.into(),
                created_at: Utc::now(),
                pr_number,
            });
        }
    }

    pub fn remove_workstream(&mut self, branch: &str) {
        self.workstreams.retain(|w| w.branch != branch);
    }

    pub fn has_workstream(&self, branch: &str) -> bool {
        self.workstreams.iter().any(|w| w.branch == branch)
    }

}

pub fn init_repo() -> Result<RepoMetadata, VexError> {
    ensure_vex_dirs()?;

    let repo_root = git::repo_root()?;
    let repo_name = git::repo_name(&repo_root)?;

    let config_path = repo_config_path(&repo_name)?;
    if config_path.exists() {
        return Err(VexError::RepoAlreadyInitialized(repo_name));
    }

    let default_branch = git::default_branch(&repo_root)?;

    let meta = RepoMetadata {
        name: repo_name,
        path: repo_root,
        default_branch,
        workstreams: vec![],
    };
    meta.save()?;
    Ok(meta)
}

pub fn resolve_repo(repo_name: Option<&str>) -> Result<RepoMetadata, VexError> {
    match repo_name {
        Some(name) => RepoMetadata::load(name),
        None => {
            let repo_root = git::repo_root()?;
            let name = git::repo_name(&repo_root)?;
            RepoMetadata::load(&name)
        }
    }
}

pub fn list_repos() -> Result<Vec<RepoMetadata>, VexError> {
    let dir = crate::config::repos_dir()?;
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut repos = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| VexError::io(&dir, e))? {
        let entry = entry.map_err(|e| VexError::io(&dir, e))?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "yml") {
            let contents = fs::read_to_string(&path).map_err(|e| VexError::io(&path, e))?;
            if let Ok(meta) = serde_yaml::from_str::<RepoMetadata>(&contents) {
                repos.push(meta);
            }
        }
    }
    repos.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(repos)
}
