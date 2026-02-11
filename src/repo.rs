use std::fs;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::repo_config_path;
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
    // Ensure dirs exist for first-time use
    let repos_dir = crate::config::repos_dir()?;
    fs::create_dir_all(&repos_dir).map_err(|e| VexError::io(&repos_dir, e))?;

    let repo_root = git::repo_root()?;
    let repo_name = git::repo_name(&repo_root)?;

    let config_path = repo_config_path(&repo_name)?;
    if config_path.exists() {
        // Already registered, just load it
        return RepoMetadata::load(&repo_name);
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
            // If the detected name is registered, use it
            if let Ok(meta) = RepoMetadata::load(&name) {
                return Ok(meta);
            }
            // Otherwise check if cwd is inside a vex worktree
            resolve_repo_from_worktree_path()
        }
    }
}

/// When inside ~/.vex/worktrees/<repo>/<branch>/, extract repo name from the path.
fn resolve_repo_from_worktree_path() -> Result<RepoMetadata, VexError> {
    let cwd = std::env::current_dir()
        .map_err(|e| VexError::ConfigError(format!("cannot get cwd: {e}")))?;
    let cwd_str = cwd.to_string_lossy();
    let worktrees_base = crate::config::worktrees_dir()?;
    if let Some(worktrees_str) = worktrees_base.to_str()
        && let Some(suffix) = cwd_str.strip_prefix(worktrees_str)
    {
        let relative = suffix.trim_start_matches('/');
        if let Some(repo_name) = relative.split('/').next()
            && !repo_name.is_empty()
        {
            return RepoMetadata::load(repo_name);
        }
    }
    Err(VexError::NotAGitRepo)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_meta() -> RepoMetadata {
        RepoMetadata {
            name: "test-repo".into(),
            path: "/tmp/test-repo".into(),
            default_branch: "main".into(),
            workstreams: vec![],
        }
    }

    #[test]
    fn add_and_has_workstream() {
        let mut meta = make_meta();
        assert!(!meta.has_workstream("feat-a"));
        meta.add_workstream("feat-a", None);
        assert!(meta.has_workstream("feat-a"));
    }

    #[test]
    fn add_workstream_with_pr() {
        let mut meta = make_meta();
        meta.add_workstream("feat-b", Some(42));
        assert_eq!(meta.workstreams[0].pr_number, Some(42));
    }

    #[test]
    fn add_workstream_idempotent() {
        let mut meta = make_meta();
        meta.add_workstream("feat-a", None);
        meta.add_workstream("feat-a", None);
        assert_eq!(meta.workstreams.len(), 1);
    }

    #[test]
    fn remove_workstream() {
        let mut meta = make_meta();
        meta.add_workstream("feat-a", None);
        meta.add_workstream("feat-b", None);
        meta.remove_workstream("feat-a");
        assert!(!meta.has_workstream("feat-a"));
        assert!(meta.has_workstream("feat-b"));
    }

    #[test]
    fn metadata_serde_roundtrip() {
        let mut meta = make_meta();
        meta.add_workstream("feat-x", Some(99));
        let yaml = serde_yaml::to_string(&meta).unwrap();
        let parsed: RepoMetadata = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.name, "test-repo");
        assert_eq!(parsed.workstreams.len(), 1);
        assert_eq!(parsed.workstreams[0].branch, "feat-x");
        assert_eq!(parsed.workstreams[0].pr_number, Some(99));
    }

    #[test]
    fn metadata_file_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let repos_dir = tmp.path().join("repos");
        std::fs::create_dir_all(&repos_dir).unwrap();

        let mut meta = make_meta();
        meta.add_workstream("feat-a", None);

        // Write directly to file
        let path = repos_dir.join("test-repo.yml");
        let yaml = serde_yaml::to_string(&meta).unwrap();
        std::fs::write(&path, &yaml).unwrap();

        // Read back
        let contents = std::fs::read_to_string(&path).unwrap();
        let loaded: RepoMetadata = serde_yaml::from_str(&contents).unwrap();
        assert_eq!(loaded.name, "test-repo");
        assert!(loaded.has_workstream("feat-a"));
    }
}
