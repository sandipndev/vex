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

#[cfg(test)]
mod tests {
    use super::{RepoStore, gen_id, next_agent_id};
    use vex_cli::{
        Agent, AgentStatus, Repository, ShellSession, ShellStatus, Workstream, WorkstreamStatus,
    };

    fn make_repo(id: &str) -> Repository {
        Repository {
            id: id.to_string(),
            name: id.to_string(),
            path: format!("/tmp/{id}"),
            default_branch: "main".to_string(),
            registered_at: 0,
            workstreams: vec![],
        }
    }

    fn make_workstream(id: &str, repo_id: &str) -> Workstream {
        Workstream {
            id: id.to_string(),
            name: id.to_string(),
            repo_id: repo_id.to_string(),
            branch: "main".to_string(),
            worktree_path: format!("/tmp/wt/{id}"),
            tmux_session: format!("vex-{id}"),
            status: WorkstreamStatus::Idle,
            agents: vec![],
            shells: vec![],
            created_at: 0,
        }
    }

    fn make_agent(id: &str, ws_id: &str) -> Agent {
        Agent {
            id: id.to_string(),
            workstream_id: ws_id.to_string(),
            tmux_window: 1,
            prompt: "test".to_string(),
            status: AgentStatus::Running,
            exit_code: None,
            spawned_at: 0,
            exited_at: None,
        }
    }

    #[test]
    fn gen_id_format() {
        let id = gen_id("repo");
        assert!(id.starts_with("repo_"));
        assert_eq!(id.len(), 11); // "repo_" (5) + 6 hex
        assert!(id[5..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn gen_id_unique() {
        assert_ne!(gen_id("ws"), gen_id("ws"));
    }

    #[test]
    fn next_agent_id_sequential() {
        assert_eq!(next_agent_id(&[]), "agent_001");
        let a1 = make_agent("agent_001", "ws_x");
        assert_eq!(next_agent_id(&[a1.clone()]), "agent_002");
        let a2 = make_agent("agent_002", "ws_x");
        assert_eq!(next_agent_id(&[a1, a2]), "agent_003");
    }

    #[test]
    fn repo_store_empty_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = RepoStore::load(dir.path().join("repos.json")).unwrap();
        assert!(store.repos.is_empty());
    }

    #[test]
    fn repo_store_save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repos.json");
        {
            let mut store = RepoStore::load(path.clone()).unwrap();
            store.repos.push(make_repo("repo_abc"));
            store.save().unwrap();
        }
        let store2 = RepoStore::load(path).unwrap();
        assert_eq!(store2.repos.len(), 1);
        assert_eq!(store2.repos[0].id, "repo_abc");
    }

    #[test]
    fn find_by_path_and_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = RepoStore::load(dir.path().join("repos.json")).unwrap();
        store.repos.push(make_repo("repo_abc"));

        assert!(store.find_by_path("/tmp/repo_abc").is_some());
        assert!(store.find_by_path("/tmp/other").is_none());
        assert!(store.find_by_id("repo_abc").is_some());
        assert!(store.find_by_id("repo_xyz").is_none());
    }

    #[test]
    fn ws_indices_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = RepoStore::load(dir.path().join("repos.json")).unwrap();
        let mut repo = make_repo("repo_abc");
        repo.workstreams.push(make_workstream("ws_111", "repo_abc"));
        repo.workstreams.push(make_workstream("ws_222", "repo_abc"));
        store.repos.push(repo);

        assert_eq!(store.ws_indices("ws_111"), Some((0, 0)));
        assert_eq!(store.ws_indices("ws_222"), Some((0, 1)));
        assert!(store.ws_indices("ws_999").is_none());
    }

    #[test]
    fn agent_indices_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = RepoStore::load(dir.path().join("repos.json")).unwrap();
        let mut repo = make_repo("repo_abc");
        let mut ws = make_workstream("ws_111", "repo_abc");
        ws.agents.push(make_agent("agent_001", "ws_111"));
        repo.workstreams.push(ws);
        store.repos.push(repo);

        assert_eq!(store.agent_indices("agent_001"), Some((0, 0, 0)));
        assert!(store.agent_indices("agent_999").is_none());
    }

    #[test]
    fn shell_indices_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = RepoStore::load(dir.path().join("repos.json")).unwrap();
        let mut repo = make_repo("repo_abc");
        let mut ws = make_workstream("ws_111", "repo_abc");
        ws.shells.push(ShellSession {
            id: "sh_aabbcc".to_string(),
            workstream_id: "ws_111".to_string(),
            tmux_window: 0,
            status: ShellStatus::Active,
            started_at: 0,
            exited_at: None,
            exit_code: None,
        });
        repo.workstreams.push(ws);
        store.repos.push(repo);

        assert_eq!(store.shell_indices("sh_aabbcc"), Some((0, 0, 0)));
        assert!(store.shell_indices("sh_999999").is_none());
    }

    #[test]
    fn refresh_ws_status_idle_when_no_running_agents() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = RepoStore::load(dir.path().join("repos.json")).unwrap();
        let mut repo = make_repo("repo_abc");
        let mut ws = make_workstream("ws_111", "repo_abc");
        ws.status = WorkstreamStatus::Running;
        let mut agent = make_agent("agent_001", "ws_111");
        agent.status = AgentStatus::Exited;
        ws.agents.push(agent);
        repo.workstreams.push(ws);
        store.repos.push(repo);

        store.refresh_ws_status("ws_111");
        assert_eq!(store.repos[0].workstreams[0].status, WorkstreamStatus::Idle);
    }

    #[test]
    fn refresh_ws_status_running_when_agent_active() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = RepoStore::load(dir.path().join("repos.json")).unwrap();
        let mut repo = make_repo("repo_abc");
        let mut ws = make_workstream("ws_111", "repo_abc");
        ws.agents.push(make_agent("agent_001", "ws_111")); // status = Running
        repo.workstreams.push(ws);
        store.repos.push(repo);

        store.refresh_ws_status("ws_111");
        assert_eq!(
            store.repos[0].workstreams[0].status,
            WorkstreamStatus::Running
        );
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
