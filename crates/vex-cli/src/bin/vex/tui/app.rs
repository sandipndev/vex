use std::time::Instant;

use vex_cli::{AgentStatus, Repository, ShellStatus, WorkstreamStatus};

use crate::config::ConnectionEntry;

// ── App mode ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    /// User is typing a task prompt after pressing `a`
    SpawnInput,
    /// After spawn: ask whether to attach to the new window
    ConfirmAttach {
        ws_id: String,
        window_index: u32,
    },
    /// User pressed `d` — waiting for confirmation
    ConfirmDelete,
    /// User pressed `c` with multiple repos — picking which repo to create in
    CreateSelectRepo {
        selected: usize,
    },
    /// Step 1: user is typing the workstream name
    CreateNameInput {
        repo_id: String,
        repo_name: String,
        default_branch: String,
    },
    /// Step 2: user is typing a branch name (empty = use default_branch)
    CreateBranchInput {
        repo_id: String,
        repo_name: String,
        name: String,
        default_branch: String,
    },
    /// Step 3: optional start-point (tag, commit, or branch) for `git worktree add -b`
    CreateFromRefInput {
        repo_id: String,
        repo_name: String,
        name: String,
        /// The branch typed in step 2 (None = use repo default)
        branch: Option<String>,
    },
    /// Step 4: confirm whether to fetch from origin
    CreateConfirmFetch {
        repo_id: String,
        name: String,
        /// None means use the repo's default branch
        branch: Option<String>,
        /// Explicit start point: tag, commit, or branch to create `branch` from
        from_ref: Option<String>,
    },
}

// ── App ───────────────────────────────────────────────────────────────────────

pub struct App {
    /// Latest snapshot of repos/workstreams/agents from vexd
    pub repos: Vec<Repository>,
    /// Currently selected workstream (global flat index)
    pub selected_ws: usize,
    /// Whether the connection is local (Unix socket) — affects attach
    pub is_local: bool,
    /// Connection entry for opening additional connections (e.g. shell attach)
    pub conn_entry: ConnectionEntry,
    /// Connection label shown in the header
    pub conn_label: String,
    /// Time of last successful refresh
    pub last_refresh: Instant,
    /// Current interaction mode
    pub mode: Mode,
    /// Text being typed in SpawnInput mode
    pub spawn_input: String,
    /// Text being typed in CreateBranchInput mode
    pub create_input: String,
    /// One-line status / error message shown at bottom
    pub status_msg: Option<String>,
}

impl App {
    pub fn new(is_local: bool, conn_label: String, conn_entry: ConnectionEntry) -> Self {
        Self {
            repos: Vec::new(),
            selected_ws: 0,
            is_local,
            conn_entry,
            conn_label,
            last_refresh: Instant::now(),
            mode: Mode::Normal,
            spawn_input: String::new(),
            create_input: String::new(),
            status_msg: None,
        }
    }

    // ── Navigation ────────────────────────────────────────────────────────────

    /// Flat list of (repo_idx, ws_idx) for all workstreams.
    pub fn ws_positions(&self) -> Vec<(usize, usize)> {
        self.repos
            .iter()
            .enumerate()
            .flat_map(|(ri, repo)| (0..repo.workstreams.len()).map(move |wi| (ri, wi)))
            .collect()
    }

    pub fn total_workstreams(&self) -> usize {
        self.repos.iter().map(|r| r.workstreams.len()).sum()
    }

    pub fn move_up(&mut self) {
        if self.selected_ws > 0 {
            self.selected_ws -= 1;
        }
    }

    pub fn move_down(&mut self) {
        let max = self.total_workstreams().saturating_sub(1);
        if self.selected_ws < max {
            self.selected_ws += 1;
        }
    }

    /// Returns the selected workstream's (repo_idx, ws_idx) if any.
    pub fn selected(&self) -> Option<(usize, usize)> {
        let pos = self.ws_positions();
        pos.get(self.selected_ws).copied()
    }

    pub fn selected_ws_id(&self) -> Option<String> {
        let (ri, wi) = self.selected()?;
        Some(self.repos[ri].workstreams[wi].id.clone())
    }

    pub fn selected_tmux_session(&self) -> Option<String> {
        let (ri, wi) = self.selected()?;
        Some(self.repos[ri].workstreams[wi].tmux_session.clone())
    }

    /// Returns the ID of the first non-Exited shell in the selected workstream.
    pub fn selected_first_shell_id(&self) -> Option<String> {
        let (ri, wi) = self.selected()?;
        self.repos[ri].workstreams[wi]
            .shells
            .iter()
            .find(|s| s.status != ShellStatus::Exited)
            .map(|s| s.id.clone())
    }

    // ── Data helpers ──────────────────────────────────────────────────────────

    pub fn update_repos(&mut self, repos: Vec<Repository>) {
        // Keep selection in bounds
        let total: usize = repos.iter().map(|r| r.workstreams.len()).sum();
        if total == 0 {
            self.selected_ws = 0;
        } else if self.selected_ws >= total {
            self.selected_ws = total - 1;
        }
        self.repos = repos;
        self.last_refresh = Instant::now();
    }

    #[allow(dead_code)]
    pub fn total_running_agents(&self) -> usize {
        self.repos
            .iter()
            .flat_map(|r| &r.workstreams)
            .flat_map(|ws| &ws.agents)
            .filter(|a| a.status == AgentStatus::Running)
            .count()
    }
}

// ── Display helpers (used by ui.rs) ──────────────────────────────────────────

pub fn ws_status_str(status: &WorkstreamStatus) -> &'static str {
    match status {
        WorkstreamStatus::Running => "Running",
        WorkstreamStatus::Idle => "Idle",
        WorkstreamStatus::Stopped => "Stopped",
    }
}

pub fn running_agents_count(ws: &vex_cli::Workstream) -> usize {
    ws.agents
        .iter()
        .filter(|a| a.status == AgentStatus::Running)
        .count()
}

pub fn format_ago(ts: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let secs = now.saturating_sub(ts);
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else {
        format!("{}h ago", secs / 3600)
    }
}
