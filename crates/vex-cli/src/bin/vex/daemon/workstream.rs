use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, bail};
use chrono::Utc;
use tokio::sync::Mutex;
use vex_cli::proto::WorkstreamInfo;

pub type WorkstreamStore = Arc<Mutex<WorkstreamStoreInner>>;

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct WorkstreamData {
    worktree_path: PathBuf,
    repo_path: PathBuf,
    branch: String,
    created_at: chrono::DateTime<Utc>,
}

pub struct WorkstreamStoreInner {
    // repo_name -> workstream_name -> data
    workstreams: HashMap<String, HashMap<String, WorkstreamData>>,
    persist_path: PathBuf,
    workstreams_base: PathBuf,
}

impl WorkstreamStoreInner {
    pub fn load(vex_dir: &Path) -> Self {
        let persist_path = vex_dir.join("workstreams.json");
        let workstreams_base = vex_dir.join("workstreams");
        let workstreams = std::fs::read_to_string(&persist_path)
            .ok()
            .and_then(|data| serde_json::from_str(&data).ok())
            .unwrap_or_default();
        Self {
            workstreams,
            persist_path,
            workstreams_base,
        }
    }

    pub fn create(&mut self, repo_name: &str, name: &str, repo_path: &Path) -> Result<PathBuf> {
        // Check if already exists
        if let Some(repo_ws) = self.workstreams.get(repo_name)
            && repo_ws.contains_key(name)
        {
            bail!(
                "workstream '{}' already exists for repo '{}'",
                name,
                repo_name
            );
        }

        let worktree_path = self.workstreams_base.join(repo_name).join(name);
        std::fs::create_dir_all(worktree_path.parent().unwrap())?;

        // git -C <repo_path> worktree add -b <name> <worktree_path>
        let output = std::process::Command::new("git")
            .args(["-C", &repo_path.to_string_lossy()])
            .args([
                "worktree",
                "add",
                "-b",
                name,
                &worktree_path.to_string_lossy(),
            ])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git worktree add failed: {}", stderr.trim());
        }

        let data = WorkstreamData {
            worktree_path: worktree_path.clone(),
            repo_path: repo_path.to_path_buf(),
            branch: name.to_string(),
            created_at: Utc::now(),
        };

        self.workstreams
            .entry(repo_name.to_string())
            .or_default()
            .insert(name.to_string(), data);
        self.flush()?;

        Ok(worktree_path)
    }

    pub fn remove(&mut self, repo_name: &str, name: &str) -> Result<()> {
        let data = self
            .workstreams
            .get(repo_name)
            .and_then(|ws| ws.get(name))
            .ok_or_else(|| {
                anyhow::anyhow!("workstream '{}' not found for repo '{}'", name, repo_name)
            })?
            .clone();

        // git -C <repo_path> worktree remove <worktree_path> --force
        let _ = std::process::Command::new("git")
            .args(["-C", &data.repo_path.to_string_lossy()])
            .args([
                "worktree",
                "remove",
                &data.worktree_path.to_string_lossy(),
                "--force",
            ])
            .output();

        // git -C <repo_path> branch -D <branch>
        let _ = std::process::Command::new("git")
            .args(["-C", &data.repo_path.to_string_lossy()])
            .args(["branch", "-D", &data.branch])
            .output();

        // Remove from store
        if let Some(repo_ws) = self.workstreams.get_mut(repo_name) {
            repo_ws.remove(name);
            if repo_ws.is_empty() {
                self.workstreams.remove(repo_name);
            }
        }

        // Clean up empty dirs
        let repo_dir = self.workstreams_base.join(repo_name);
        let _ = std::fs::remove_dir(&repo_dir);

        self.flush()
    }

    pub fn list(&self, repo_filter: Option<&str>) -> Vec<WorkstreamInfo> {
        let mut result = Vec::new();
        for (repo_name, ws_map) in &self.workstreams {
            if let Some(filter) = repo_filter
                && repo_name != filter
            {
                continue;
            }
            for (ws_name, data) in ws_map {
                result.push(WorkstreamInfo {
                    repo: repo_name.clone(),
                    name: ws_name.clone(),
                    worktree_path: data.worktree_path.clone(),
                    branch: data.branch.clone(),
                    created_at: data.created_at,
                });
            }
        }
        result
    }

    pub fn get_worktree_path(&self, repo_name: &str, name: &str) -> Option<PathBuf> {
        self.workstreams
            .get(repo_name)?
            .get(name)
            .map(|d| d.worktree_path.clone())
    }

    fn flush(&self) -> Result<()> {
        let data = serde_json::to_string_pretty(&self.workstreams)?;
        std::fs::write(&self.persist_path, data)?;
        Ok(())
    }
}

pub fn new_workstream_store(vex_dir: &Path) -> WorkstreamStore {
    Arc::new(Mutex::new(WorkstreamStoreInner::load(vex_dir)))
}
