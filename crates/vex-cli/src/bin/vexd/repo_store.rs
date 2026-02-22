use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rand::RngCore;

use vex_cli::{Agent, AgentStatus, Repository, Workstream, WorkstreamStatus};

// ── Helpers ───────────────────────────────────────────────────────────────────

pub fn unix_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn gen_id(prefix: &str) -> String {
    let mut bytes = [0u8; 3];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}_{hex}")
}

pub fn next_agent_id(agents: &[Agent]) -> String {
    let n = agents.len() + 1;
    format!("agent_{n:03}")
}

// ── RepoStore ─────────────────────────────────────────────────────────────────

/// Persists all repository / workstream / agent state to `repos.json`.
pub struct RepoStore {
    path: PathBuf,
    pub repos: Vec<Repository>,
}

impl RepoStore {
    pub fn load(path: PathBuf) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                path,
                repos: Vec::new(),
            });
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let repos: Vec<Repository> =
            serde_json::from_str(&content).context("parsing repos.json")?;
        Ok(Self { path, repos })
    }

    /// Atomically write (write to .tmp, then rename).
    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("tmp");
        let content = serde_json::to_string_pretty(&self.repos)?;
        std::fs::write(&tmp, &content)?;
        std::fs::rename(&tmp, &self.path).context("renaming repos.tmp → repos.json")
    }

    pub fn find_by_path(&self, path: &str) -> Option<&Repository> {
        self.repos.iter().find(|r| r.path == path)
    }

    pub fn find_by_id(&self, id: &str) -> Option<&Repository> {
        self.repos.iter().find(|r| r.id == id)
    }

    pub fn find_by_id_mut(&mut self, id: &str) -> Option<&mut Repository> {
        self.repos.iter_mut().find(|r| r.id == id)
    }

    // Returns (repo_idx, ws_idx) for the given workstream ID.
    pub fn ws_indices(&self, ws_id: &str) -> Option<(usize, usize)> {
        for (ri, repo) in self.repos.iter().enumerate() {
            for (wi, ws) in repo.workstreams.iter().enumerate() {
                if ws.id == ws_id {
                    return Some((ri, wi));
                }
            }
        }
        None
    }

    // Returns (repo_idx, ws_idx, agent_idx).
    pub fn agent_indices(&self, agent_id: &str) -> Option<(usize, usize, usize)> {
        for (ri, repo) in self.repos.iter().enumerate() {
            for (wi, ws) in repo.workstreams.iter().enumerate() {
                for (ai, agent) in ws.agents.iter().enumerate() {
                    if agent.id == agent_id {
                        return Some((ri, wi, ai));
                    }
                }
            }
        }
        None
    }

    // Returns (repo_idx, ws_idx, shell_idx).
    pub fn shell_indices(&self, shell_id: &str) -> Option<(usize, usize, usize)> {
        for (ri, repo) in self.repos.iter().enumerate() {
            for (wi, ws) in repo.workstreams.iter().enumerate() {
                for (si, sh) in ws.shells.iter().enumerate() {
                    if sh.id == shell_id {
                        return Some((ri, wi, si));
                    }
                }
            }
        }
        None
    }

    /// Snapshot of a workstream (cheap clone for the response).
    pub fn get_workstream(&self, ws_id: &str) -> Option<&Workstream> {
        let (ri, wi) = self.ws_indices(ws_id)?;
        Some(&self.repos[ri].workstreams[wi])
    }

    /// Recompute and update a workstream's status based on its agents.
    pub fn refresh_ws_status(&mut self, ws_id: &str) {
        if let Some((ri, wi)) = self.ws_indices(ws_id) {
            let ws = &mut self.repos[ri].workstreams[wi];
            let any_running = ws.agents.iter().any(|a| a.status == AgentStatus::Running);
            if ws.status != WorkstreamStatus::Stopped {
                ws.status = if any_running {
                    WorkstreamStatus::Running
                } else {
                    WorkstreamStatus::Idle
                };
            }
        }
    }

    /// Reconcile state on daemon start: mark workstreams Stopped and agents
    /// Exited if their tmux session is gone. Returns list of (ws_id, agent_id)
    /// pairs that are still Running (for monitor restart).
    pub fn reconcile(
        &mut self,
        alive_sessions: &std::collections::HashSet<String>,
    ) -> Vec<(String, String, u32)> {
        let mut still_running = Vec::new();
        for repo in &mut self.repos {
            for ws in &mut repo.workstreams {
                if ws.status == WorkstreamStatus::Stopped {
                    continue;
                }
                if !alive_sessions.contains(&ws.tmux_session) {
                    ws.status = WorkstreamStatus::Stopped;
                    for agent in &mut ws.agents {
                        if agent.status == AgentStatus::Running {
                            agent.status = AgentStatus::Exited;
                            agent.exited_at = Some(unix_ts());
                        }
                    }
                } else {
                    // Session alive — collect still-running agents for monitor restart
                    for agent in &ws.agents {
                        if agent.status == AgentStatus::Running {
                            still_running.push((
                                ws.id.clone(),
                                agent.id.clone(),
                                agent.tmux_window,
                            ));
                        }
                    }
                }
            }
        }
        still_running
    }
}

/// Check whether a worktree path still exists; just logs a warning if missing.
pub fn warn_missing_worktrees(repos: &[Repository]) {
    for repo in repos {
        for ws in &repo.workstreams {
            if !Path::new(&ws.worktree_path).exists() {
                tracing::warn!("Worktree missing for {}: {}", ws.id, ws.worktree_path);
            }
        }
    }
}
