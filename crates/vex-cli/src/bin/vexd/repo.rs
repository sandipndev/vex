use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkstreamEntry {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoEntry {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub workstreams: Vec<WorkstreamEntry>,
}

pub struct RepoStore {
    path: PathBuf,
    repos: Vec<RepoEntry>,
}

impl RepoStore {
    pub fn load(path: PathBuf) -> Result<Self> {
        if path.exists() {
            let data = std::fs::read_to_string(&path)?;
            let repos: Vec<RepoEntry> = serde_json::from_str(&data)?;
            Ok(Self { path, repos })
        } else {
            Ok(Self {
                path,
                repos: vec![],
            })
        }
    }

    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(&self.repos)?;
        std::fs::write(&self.path, data)?;
        Ok(())
    }

    pub fn register(&mut self, name: String, path: String) -> Result<RepoEntry> {
        // Reject duplicate names
        if self.repos.iter().any(|r| r.name == name) {
            anyhow::bail!("repository '{}' is already registered", name);
        }

        // Resolve to absolute and validate
        let resolved = std::path::Path::new(&path)
            .canonicalize()
            .map_err(|e| anyhow::anyhow!("path '{}': {}", path, e))?;
        if !resolved.is_dir() {
            anyhow::bail!("path '{}' is not a directory", path);
        }

        let entry = RepoEntry {
            name,
            path: resolved.to_string_lossy().to_string(),
            workstreams: vec![],
        };
        self.repos.push(entry.clone());
        self.save()?;
        Ok(entry)
    }

    pub fn unregister(&mut self, name: &str) -> bool {
        let before = self.repos.len();
        self.repos.retain(|r| r.name != name);
        let removed = self.repos.len() < before;
        if removed {
            let _ = self.save();
        }
        removed
    }

    pub fn list(&self) -> &[RepoEntry] {
        &self.repos
    }

    pub fn create_workstream(&mut self, repo_name: &str, ws_name: String) -> Result<()> {
        let repo = self
            .repos
            .iter_mut()
            .find(|r| r.name == repo_name)
            .ok_or_else(|| anyhow::anyhow!("repository '{}' not found", repo_name))?;
        if repo.workstreams.iter().any(|w| w.name == ws_name) {
            anyhow::bail!(
                "workstream '{}' already exists in repo '{}'",
                ws_name,
                repo_name
            );
        }
        repo.workstreams.push(WorkstreamEntry { name: ws_name });
        self.save()?;
        Ok(())
    }

    pub fn list_workstreams(&self, repo_name: &str) -> Result<&[WorkstreamEntry]> {
        let repo = self
            .repos
            .iter()
            .find(|r| r.name == repo_name)
            .ok_or_else(|| anyhow::anyhow!("repository '{}' not found", repo_name))?;
        Ok(&repo.workstreams)
    }

    pub fn delete_workstream(&mut self, repo_name: &str, ws_name: &str) -> Result<bool> {
        let repo = self
            .repos
            .iter_mut()
            .find(|r| r.name == repo_name)
            .ok_or_else(|| anyhow::anyhow!("repository '{}' not found", repo_name))?;
        let before = repo.workstreams.len();
        repo.workstreams.retain(|w| w.name != ws_name);
        let removed = repo.workstreams.len() < before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }
}
