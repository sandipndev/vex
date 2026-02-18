use std::collections::HashMap;
use std::io;
use std::process::Command;
use std::time::{Duration, Instant};

use ansi_to_tui::IntoText as _;
use chrono::{DateTime, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::config::Config;
use crate::error::VexError;
use crate::git;
use crate::repo;
use crate::tmux;
use crate::worker::{Worker, WorkerRequest, WorkerResponse};
use crate::workstream;

struct WorkstreamItem {
    repo_name: String,
    branch: String,
    session: String,
    active: bool,
    pr_number: Option<u64>,
    repo_path: String,
    last_accessed_at: Option<DateTime<Utc>>,
    _created_at: DateTime<Utc>,
}

#[derive(PartialEq, Clone)]
enum FocusPane {
    Left,
    Right,
}

enum ListRow {
    RepoHeader { repo_name: String },
    Workstream { index: usize },
}

struct GithubPrData {
    title: String,
    number: u64,
    body: String,
    url: String,
    state: String,
    comments: Vec<(String, String)>,        // (author, body)
    reviews: Vec<(String, String, String)>, // (author, body, state)
    checks_passed: u32,
    checks_total: u32,
}

#[derive(PartialEq)]
enum InputMode {
    Normal,
    Interact,
    CreateNew,
    SelectBaseBranch,
    Rename,
    ConfirmDelete,
    ConfirmDeactivate,
}

struct LoadingState {
    workstreams: bool,
    preview: bool,
    pr_cache: bool,
    pr_structured: bool,
    branches: bool,
}

impl LoadingState {
    fn new() -> Self {
        LoadingState {
            workstreams: false,
            preview: false,
            pr_cache: false,
            pr_structured: false,
            branches: false,
        }
    }
}

struct App {
    workstreams: Vec<WorkstreamItem>,
    list_state: ListState,
    input_mode: InputMode,
    input_buffer: String,
    input_cursor: usize,
    preview_content: String,
    config: Config,
    should_switch: Option<String>,
    status_message: Option<(String, Instant)>,
    should_quit: bool,
    // Branch picker state for two-step create flow
    pending_new_branch: String,
    branch_candidates: Vec<String>,
    branch_filter: String,
    branch_list_state: ListState,
    // Focus and scroll
    focus: FocusPane,
    github_pr_data: Option<GithubPrData>,
    github_expanded: bool,
    github_scroll: u16,
    tmux_scroll: u16,
    // Grouped list
    visible_rows: Vec<ListRow>,
    // Help overlay
    show_help: bool,
    // PR cache
    pr_cache: HashMap<String, Option<u64>>,
    // Background worker
    worker: Option<Worker>,
    loading: LoadingState,
    initial_load_done: bool,
    /// After create/rename, select this branch when workstreams refresh
    pending_select_branch: Option<String>,
    // Background git fetch
    last_fetch: Instant,
}

impl App {
    fn new(config: Config) -> Self {
        let mut worker = Worker::spawn();
        // Fire off initial async request instead of blocking
        worker.send(WorkerRequest::RefreshWorkstreams);

        App {
            workstreams: Vec::new(),
            list_state: ListState::default(),
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            input_cursor: 0,
            preview_content: String::new(),
            config,
            should_switch: None,
            status_message: None,
            should_quit: false,
            pending_new_branch: String::new(),
            branch_candidates: Vec::new(),
            branch_filter: String::new(),
            branch_list_state: ListState::default(),
            focus: FocusPane::Left,
            github_pr_data: None,
            github_expanded: false,
            github_scroll: 0,
            tmux_scroll: 0,
            visible_rows: Vec::new(),
            show_help: false,
            pr_cache: HashMap::new(),
            worker: Some(worker),
            loading: LoadingState {
                workstreams: true,
                ..LoadingState::new()
            },
            initial_load_done: false,
            pending_select_branch: None,
            last_fetch: Instant::now(),
        }
    }

    /// Build visible_rows from workstreams, grouped by repo and sorted by LRU.
    fn rebuild_visible_rows(&mut self) {
        // Group workstreams by repo_name
        let mut repo_groups: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, ws) in self.workstreams.iter().enumerate() {
            repo_groups.entry(ws.repo_name.clone()).or_default().push(i);
        }

        // Sort repos by most-recent last_accessed_at (descending)
        let mut repos: Vec<(String, Vec<usize>)> = repo_groups.into_iter().collect();
        repos.sort_by(|(_, a_indices), (_, b_indices)| {
            let a_max = a_indices
                .iter()
                .filter_map(|&i| self.workstreams[i].last_accessed_at)
                .max();
            let b_max = b_indices
                .iter()
                .filter_map(|&i| self.workstreams[i].last_accessed_at)
                .max();
            b_max.cmp(&a_max)
        });

        self.visible_rows.clear();
        for (repo_name, mut indices) in repos {
            // Sort workstreams within repo by last_accessed_at descending
            indices.sort_by(|&a, &b| {
                let a_time = self.workstreams[a].last_accessed_at;
                let b_time = self.workstreams[b].last_accessed_at;
                b_time.cmp(&a_time)
            });
            self.visible_rows.push(ListRow::RepoHeader { repo_name });
            for idx in indices {
                self.visible_rows.push(ListRow::Workstream { index: idx });
            }
        }
    }

    /// Apply workstream data from worker response, preserving selection.
    fn apply_workstream_data(&mut self, items: Vec<crate::worker::WorkstreamData>) {
        let old_selected = self.selected_session();

        self.workstreams = items
            .into_iter()
            .map(|d| {
                let key = format!("{}/{}", d.repo_path, d.branch);
                let pr_number = self.pr_cache.get(&key).copied().flatten();
                WorkstreamItem {
                    repo_name: d.repo_name,
                    branch: d.branch,
                    session: d.session,
                    active: d.active,
                    pr_number,
                    repo_path: d.repo_path,
                    last_accessed_at: d.last_accessed_at,
                    _created_at: d.created_at,
                }
            })
            .collect();

        self.rebuild_visible_rows();

        // If there's a pending branch selection (from create/rename), prefer that
        if let Some(branch) = self.pending_select_branch.take()
            && let Some(row_idx) = self.find_row_for_branch(&branch)
        {
            self.list_state.select(Some(row_idx));
            self.request_pr_data();
            return;
        }

        // Try to preserve selection by session name
        if let Some(old_session) = old_selected
            && let Some(row_idx) = self.visible_rows.iter().position(|row| {
                matches!(row, ListRow::Workstream { index } if self.workstreams[*index].session == old_session)
            })
        {
            self.list_state.select(Some(row_idx));
            self.request_pr_data();
            return;
        }
        // Fallback: select first workstream row
        if let Some(first_ws) = self
            .visible_rows
            .iter()
            .position(|r| matches!(r, ListRow::Workstream { .. }))
        {
            self.list_state.select(Some(first_ws));
        } else {
            self.list_state.select(None);
        }
        self.request_pr_data();
    }

    fn find_row_for_branch(&self, branch: &str) -> Option<usize> {
        self.visible_rows.iter().position(|row| {
            matches!(row, ListRow::Workstream { index } if self.workstreams[*index].branch == branch)
        })
    }

    /// Process all pending worker responses. Called at top of each run_loop iteration.
    fn process_worker_responses(&mut self) {
        let worker = match self.worker.as_mut() {
            Some(w) => w,
            None => return,
        };
        let responses = worker.try_recv_all();

        for resp in responses {
            match resp {
                WorkerResponse::WorkstreamsRefreshed { items } => {
                    self.loading.workstreams = false;
                    self.apply_workstream_data(items);

                    if !self.initial_load_done {
                        self.initial_load_done = true;
                        if !self.workstreams.is_empty() && self.list_state.selected().is_none() {
                            // Select first workstream row
                            if let Some(first_ws) = self
                                .visible_rows
                                .iter()
                                .position(|r| matches!(r, ListRow::Workstream { .. }))
                            {
                                self.list_state.select(Some(first_ws));
                            }
                        }
                        // Trigger PR cache load on first load
                        let mut repo_paths: Vec<String> = self
                            .workstreams
                            .iter()
                            .map(|w| w.repo_path.clone())
                            .collect();
                        repo_paths.sort();
                        repo_paths.dedup();
                        if !repo_paths.is_empty()
                            && let Some(w) = self.worker.as_mut()
                            && w.send(WorkerRequest::LoadPrCache {
                                repo_paths: repo_paths.clone(),
                            })
                        {
                            self.loading.pr_cache = true;
                        }
                        // Fire initial git fetch
                        if !repo_paths.is_empty()
                            && let Some(w) = self.worker.as_mut()
                        {
                            w.send(WorkerRequest::GitFetch { repo_paths });
                            self.last_fetch = Instant::now();
                        }
                    }

                    // Request pane capture for selected workstream
                    self.request_capture_pane();
                }

                WorkerResponse::PaneCaptured {
                    session,
                    window,
                    content,
                } => {
                    self.loading.preview = false;
                    // Only update if it still matches the selected workstream
                    let matches = self
                        .selected()
                        .is_some_and(|ws| ws.session == session && ws.active)
                        && window == self.config.default_window;
                    if matches {
                        self.preview_content = content;
                    }
                }

                WorkerResponse::PrCacheLoaded { entries } => {
                    self.loading.pr_cache = false;
                    for (key, num) in entries {
                        self.pr_cache.insert(key, Some(num));
                    }
                    // Update pr_number on all workstream items
                    for ws in &mut self.workstreams {
                        let key = format!("{}/{}", ws.repo_path, ws.branch);
                        ws.pr_number = self.pr_cache.get(&key).copied().flatten();
                    }
                    // Trigger PR data load for currently selected
                    self.request_pr_data();
                }

                WorkerResponse::PrStructuredFetched { pr_number, data } => {
                    self.loading.pr_structured = false;
                    // Only apply if still matches selected workstream
                    let matches = self
                        .selected()
                        .and_then(|ws| ws.pr_number)
                        .is_some_and(|n| n == pr_number);
                    if matches {
                        match data {
                            Ok(json) => {
                                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&json)
                                {
                                    self.github_pr_data = Some(GithubPrData {
                                        title: parsed["title"].as_str().unwrap_or("").to_string(),
                                        number: parsed["number"].as_u64().unwrap_or(0),
                                        body: parsed["body"].as_str().unwrap_or("").to_string(),
                                        url: parsed["url"].as_str().unwrap_or("").to_string(),
                                        state: parsed["state"].as_str().unwrap_or("").to_string(),
                                        comments: parsed["comments"]
                                            .as_array()
                                            .map(|arr| {
                                                arr.iter()
                                                    .map(|c| {
                                                        (
                                                            c["author"]
                                                                .as_str()
                                                                .unwrap_or("")
                                                                .to_string(),
                                                            c["body"]
                                                                .as_str()
                                                                .unwrap_or("")
                                                                .to_string(),
                                                        )
                                                    })
                                                    .collect()
                                            })
                                            .unwrap_or_default(),
                                        reviews: parsed["reviews"]
                                            .as_array()
                                            .map(|arr| {
                                                arr.iter()
                                                    .map(|r| {
                                                        (
                                                            r["author"]
                                                                .as_str()
                                                                .unwrap_or("")
                                                                .to_string(),
                                                            r["body"]
                                                                .as_str()
                                                                .unwrap_or("")
                                                                .to_string(),
                                                            r["state"]
                                                                .as_str()
                                                                .unwrap_or("")
                                                                .to_string(),
                                                        )
                                                    })
                                                    .collect()
                                            })
                                            .unwrap_or_default(),
                                        checks_passed: parsed["checks_passed"].as_u64().unwrap_or(0)
                                            as u32,
                                        checks_total: parsed["checks_total"].as_u64().unwrap_or(0)
                                            as u32,
                                    });
                                }
                            }
                            Err(e) => {
                                self.set_status(format!("PR fetch error: {e}"));
                            }
                        }
                    }
                }

                WorkerResponse::BranchesListed { branches } => {
                    self.loading.branches = false;
                    // Only transition if we're still waiting for branches
                    if !self.pending_new_branch.is_empty() {
                        self.branch_candidates = branches;
                        self.branch_list_state
                            .select(if self.branch_candidates.is_empty() {
                                None
                            } else {
                                Some(0)
                            });
                        self.input_mode = InputMode::SelectBaseBranch;
                    }
                }

                WorkerResponse::GitFetchCompleted => {
                    // Reload PR cache after fetch
                    let mut repo_paths: Vec<String> = self
                        .workstreams
                        .iter()
                        .map(|w| w.repo_path.clone())
                        .collect();
                    repo_paths.sort();
                    repo_paths.dedup();
                    if !repo_paths.is_empty()
                        && let Some(w) = self.worker.as_mut()
                    {
                        w.send(WorkerRequest::LoadPrCache { repo_paths });
                    }
                }
            }
        }
    }

    /// Send a CapturePane request if appropriate for the current selection.
    fn request_capture_pane(&mut self) {
        let ws_info = self.selected().map(|ws| (ws.session.clone(), ws.active));
        let default_window = self.config.default_window.clone();

        if let Some((session, active)) = ws_info {
            if active {
                if let Some(w) = self.worker.as_mut()
                    && w.send(WorkerRequest::CapturePane {
                        session,
                        window: default_window,
                    })
                {
                    self.loading.preview = true;
                }
            } else {
                self.preview_content = "(session not active)".into();
            }
        } else {
            self.preview_content = "(no workstreams)".into();
        }
    }

    /// Request structured PR data for the currently selected workstream.
    fn request_pr_data(&mut self) {
        let info = self
            .selected()
            .and_then(|ws| ws.pr_number.map(|n| (ws.repo_path.clone(), n)));
        if let Some((repo_path, pr_number)) = info {
            if let Some(w) = self.worker.as_mut()
                && w.send(WorkerRequest::FetchPrStructured {
                    repo_path,
                    pr_number,
                })
            {
                self.loading.pr_structured = true;
            }
        } else {
            self.github_pr_data = None;
        }
    }

    fn selected(&self) -> Option<&WorkstreamItem> {
        self.list_state
            .selected()
            .and_then(|i| self.visible_rows.get(i))
            .and_then(|row| match row {
                ListRow::Workstream { index } => self.workstreams.get(*index),
                _ => None,
            })
    }

    fn selected_session(&self) -> Option<String> {
        self.selected().map(|w| w.session.clone())
    }

    fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some((msg.into(), Instant::now()));
    }

    fn move_up(&mut self) {
        if self.visible_rows.is_empty() {
            return;
        }
        let current = self.list_state.selected().unwrap_or(0);
        // Find next workstream row going up (wrapping)
        let len = self.visible_rows.len();
        let mut i = if current == 0 { len - 1 } else { current - 1 };
        for _ in 0..len {
            if matches!(self.visible_rows[i], ListRow::Workstream { .. }) {
                self.list_state.select(Some(i));
                self.github_scroll = 0;
                self.tmux_scroll = 0;
                self.github_pr_data = None;
                self.request_pr_data();
                self.request_capture_pane();
                return;
            }
            i = if i == 0 { len - 1 } else { i - 1 };
        }
    }

    fn move_down(&mut self) {
        if self.visible_rows.is_empty() {
            return;
        }
        let current = self.list_state.selected().unwrap_or(0);
        let len = self.visible_rows.len();
        let mut i = if current + 1 >= len { 0 } else { current + 1 };
        for _ in 0..len {
            if matches!(self.visible_rows[i], ListRow::Workstream { .. }) {
                self.list_state.select(Some(i));
                self.github_scroll = 0;
                self.tmux_scroll = 0;
                self.github_pr_data = None;
                self.request_pr_data();
                self.request_capture_pane();
                return;
            }
            i = if i + 1 >= len { 0 } else { i + 1 };
        }
    }

    fn enter_interact(&mut self) {
        if let Some(ws) = self.selected() {
            if !ws.active {
                self.set_status("Cannot interact: session not active");
                return;
            }
        } else {
            return;
        }
        self.focus = FocusPane::Right;
        self.input_mode = InputMode::Interact;
    }

    fn handle_open(&mut self) {
        if let Some(ws) = self.selected() {
            let session = ws.session.clone();
            let repo_name = ws.repo_name.clone();
            let branch = ws.branch.clone();
            // If session isn't active, recreate it
            if !ws.active
                && let Ok(worktree_dir) = crate::config::repo_worktree_dir(&repo_name)
            {
                let worktree_path = worktree_dir.join(&branch);
                let worktree_str = worktree_path.to_string_lossy().to_string();
                if let Err(e) = tmux::create_session(&session, &worktree_str, &self.config) {
                    self.set_status(format!("Error recreating session: {e}"));
                    return;
                }
            }
            // Touch LRU
            if let Ok(mut meta) = repo::resolve_repo(Some(&repo_name)) {
                meta.touch_workstream(&branch);
                let _ = meta.save();
            }
            self.should_switch = Some(session);
            self.should_quit = true;
        }
    }

    fn send_key_to_tmux(&self, key_str: &str) {
        if let Some(ws) = self.selected() {
            let target = format!("{}:{}", ws.session, self.config.default_window);
            let _ = Command::new("tmux")
                .args(["send-keys", "-t", &target, key_str])
                .output();
        }
    }

    fn start_create(&mut self) {
        self.input_mode = InputMode::CreateNew;
        self.input_buffer.clear();
        self.input_cursor = 0;
    }

    fn confirm_create(&mut self) {
        let branch = self.input_buffer.trim().to_string();
        if branch.is_empty() {
            self.input_mode = InputMode::Normal;
            self.set_status("Cancelled: empty branch name");
            return;
        }
        // Save branch name and transition to base branch picker
        self.pending_new_branch = branch;
        self.branch_filter.clear();

        // Determine repo path for listing branches
        let repo_path = repo::list_repos()
            .ok()
            .and_then(|repos| repos.into_iter().next())
            .map(|r| r.path)
            .or_else(|| git::repo_root().ok());

        let Some(path) = repo_path else {
            // No repo found, just create with default base
            self.do_create_workstream(&self.pending_new_branch.clone(), None);
            return;
        };

        // Send async request for branches
        self.set_status("Loading branches...");
        if let Some(w) = self.worker.as_mut()
            && w.send(WorkerRequest::ListBranches { repo_path: path })
        {
            self.loading.branches = true;
        }
    }

    fn filtered_branches(&self) -> Vec<(usize, &str)> {
        self.branch_candidates
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                self.branch_filter.is_empty()
                    || b.to_lowercase()
                        .contains(&self.branch_filter.to_lowercase())
            })
            .map(|(i, b)| (i, b.as_str()))
            .collect()
    }

    fn confirm_base_branch(&mut self) {
        let filtered = self.filtered_branches();
        let selected = self
            .branch_list_state
            .selected()
            .and_then(|i| filtered.get(i))
            .map(|(_, b)| b.to_string());

        let branch = self.pending_new_branch.clone();
        self.do_create_workstream(&branch, selected.as_deref());
    }

    fn do_create_workstream(&mut self, branch: &str, base: Option<&str>) {
        self.input_mode = InputMode::Normal;
        match workstream::create_no_attach(None, branch, base, true) {
            Ok(_session) => {
                self.set_status(format!("Created workstream '{branch}'"));
                self.pending_select_branch = Some(branch.to_string());
                if let Some(w) = self.worker.as_mut() {
                    w.send(WorkerRequest::RefreshWorkstreams);
                    self.loading.workstreams = true;
                }
            }
            Err(e) => self.set_status(format!("Error: {e}")),
        }
    }

    fn start_delete(&mut self) {
        if self.selected().is_some() {
            self.input_mode = InputMode::ConfirmDelete;
        }
    }

    fn confirm_delete(&mut self) {
        if let Some(ws) = self.selected() {
            let repo_name = ws.repo_name.clone();
            let branch = ws.branch.clone();
            self.input_mode = InputMode::Normal;
            match workstream::remove(Some(&repo_name), &branch, true) {
                Ok(()) => {
                    self.set_status(format!("Deleted workstream '{branch}'"));
                    if let Some(w) = self.worker.as_mut() {
                        w.send(WorkerRequest::RefreshWorkstreams);
                        self.loading.workstreams = true;
                    }
                }
                Err(e) => self.set_status(format!("Error: {e}")),
            }
        } else {
            self.input_mode = InputMode::Normal;
        }
    }

    fn start_deactivate(&mut self) {
        if let Some(ws) = self.selected() {
            if ws.active {
                self.input_mode = InputMode::ConfirmDeactivate;
            } else {
                self.set_status("Session is not active");
            }
        }
    }

    fn confirm_deactivate(&mut self) {
        if let Some(ws) = self.selected() {
            let session = ws.session.clone();
            let branch = ws.branch.clone();
            self.input_mode = InputMode::Normal;
            match tmux::kill_session(&session) {
                Ok(()) => {
                    self.set_status(format!("Deactivated '{branch}'"));
                    if let Some(w) = self.worker.as_mut() {
                        w.send(WorkerRequest::RefreshWorkstreams);
                        self.loading.workstreams = true;
                    }
                }
                Err(e) => self.set_status(format!("Error: {e}")),
            }
        } else {
            self.input_mode = InputMode::Normal;
        }
    }

    fn start_rename(&mut self) {
        let branch = self.selected().map(|ws| ws.branch.clone());
        if let Some(branch) = branch {
            self.input_mode = InputMode::Rename;
            self.input_buffer = branch;
            self.input_cursor = self.input_buffer.len();
        }
    }

    fn confirm_rename(&mut self) {
        let new_branch = self.input_buffer.trim().to_string();
        self.input_mode = InputMode::Normal;
        if new_branch.is_empty() {
            self.set_status("Cancelled: empty branch name");
            return;
        }
        if let Some(ws) = self.selected() {
            let repo_name = ws.repo_name.clone();
            let old_branch = ws.branch.clone();
            if new_branch == old_branch {
                self.set_status("Cancelled: same name");
                return;
            }
            match workstream::rename(Some(&repo_name), Some(&old_branch), &new_branch, true) {
                Ok(()) => {
                    self.set_status(format!("Renamed '{old_branch}' -> '{new_branch}'"));
                    self.pending_select_branch = Some(new_branch);
                    if let Some(w) = self.worker.as_mut() {
                        w.send(WorkerRequest::RefreshWorkstreams);
                        self.loading.workstreams = true;
                    }
                }
                Err(e) => self.set_status(format!("Error: {e}")),
            }
        }
    }

    fn handle_input_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char(c) => {
                self.input_buffer.insert(self.input_cursor, c);
                self.input_cursor += 1;
            }
            KeyCode::Backspace => {
                if self.input_cursor > 0 {
                    self.input_cursor -= 1;
                    self.input_buffer.remove(self.input_cursor);
                }
            }
            KeyCode::Left => {
                if self.input_cursor > 0 {
                    self.input_cursor -= 1;
                }
            }
            KeyCode::Right => {
                if self.input_cursor < self.input_buffer.len() {
                    self.input_cursor += 1;
                }
            }
            KeyCode::Home => self.input_cursor = 0,
            KeyCode::End => self.input_cursor = self.input_buffer.len(),
            _ => {}
        }
    }
}

fn key_to_tmux_send(code: KeyCode, modifiers: KeyModifiers) -> Option<String> {
    if modifiers.contains(KeyModifiers::CONTROL) {
        return match code {
            KeyCode::Char(c) => Some(format!("C-{c}")),
            _ => None,
        };
    }
    match code {
        KeyCode::Char(c) => Some(c.to_string()),
        KeyCode::Enter => Some("Enter".into()),
        KeyCode::Backspace => Some("BSpace".into()),
        KeyCode::Tab => Some("Tab".into()),
        KeyCode::Up => Some("Up".into()),
        KeyCode::Down => Some("Down".into()),
        KeyCode::Left => Some("Left".into()),
        KeyCode::Right => Some("Right".into()),
        KeyCode::Delete => Some("DC".into()),
        KeyCode::Home => Some("Home".into()),
        KeyCode::End => Some("End".into()),
        KeyCode::PageUp => Some("PPage".into()),
        KeyCode::PageDown => Some("NPage".into()),
        _ => None,
    }
}

pub fn run() -> Result<(), VexError> {
    let config = Config::load_or_create()?;
    let mut app = App::new(config);

    // Setup terminal
    terminal::enable_raw_mode()
        .map_err(|e| VexError::ConfigError(format!("failed to enable raw mode: {e}")))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)
        .map_err(|e| VexError::ConfigError(format!("failed to enter alternate screen: {e}")))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)
        .map_err(|e| VexError::ConfigError(format!("failed to create terminal: {e}")))?;

    let mut last_refresh = Instant::now();
    let refresh_interval = Duration::from_secs(2);

    let result = run_loop(&mut terminal, &mut app, &mut last_refresh, refresh_interval);

    // Shut down worker thread
    if let Some(worker) = app.worker.take() {
        worker.shutdown();
    }

    // Restore terminal
    terminal::disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    result?;

    // If user chose to switch, attach after terminal is restored
    if let Some(session) = app.should_switch {
        tmux::attach(&session)?;
    }

    Ok(())
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    last_refresh: &mut Instant,
    refresh_interval: Duration,
) -> Result<(), VexError> {
    loop {
        // Process any pending worker responses before drawing
        app.process_worker_responses();

        terminal
            .draw(|f| ui(f, app))
            .map_err(|e| VexError::ConfigError(format!("draw error: {e}")))?;

        if app.should_quit {
            break;
        }

        // Shorter poll timeout in Interact mode for snappy display
        let timeout = if app.input_mode == InputMode::Interact {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(200)
        };

        if event::poll(timeout)
            .map_err(|e| VexError::ConfigError(format!("event poll error: {e}")))?
            && let Event::Key(key) = event::read()
                .map_err(|e| VexError::ConfigError(format!("event read error: {e}")))?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            // Help overlay: any key dismisses
            if app.show_help {
                app.show_help = false;
                continue;
            }

            // Ctrl+C always quits (except in Interact mode where it's forwarded)
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && key.code == KeyCode::Char('c')
                && app.input_mode != InputMode::Interact
            {
                break;
            }

            match app.input_mode {
                InputMode::Normal => {
                    if app.focus == FocusPane::Left {
                        // Left pane focused
                        if key.modifiers.contains(KeyModifiers::CONTROL) {
                            match key.code {
                                KeyCode::Char('d') => app.start_delete(),
                                KeyCode::Char('x') => app.start_deactivate(),
                                _ => {}
                            }
                        } else {
                            match key.code {
                                KeyCode::Char('q') | KeyCode::Esc => break,
                                KeyCode::Up => app.move_up(),
                                KeyCode::Down => app.move_down(),
                                KeyCode::Enter => app.handle_open(),
                                KeyCode::Right => {
                                    app.focus = FocusPane::Right;
                                }
                                KeyCode::Char('n') => app.start_create(),
                                KeyCode::Char('r') | KeyCode::Char('s') => app.start_rename(),
                                KeyCode::Char('?') => {
                                    app.show_help = true;
                                }
                                _ => {}
                            }
                        }
                    } else {
                        // Right pane focused
                        if key.modifiers.contains(KeyModifiers::CONTROL) {
                            if let KeyCode::Char('o') = key.code {
                                app.enter_interact();
                            }
                        } else {
                            match key.code {
                                KeyCode::Up => {
                                    if app.github_expanded {
                                        app.github_scroll = app.github_scroll.saturating_sub(1);
                                    } else {
                                        app.tmux_scroll = app.tmux_scroll.saturating_sub(1);
                                    }
                                }
                                KeyCode::Down => {
                                    if app.github_expanded {
                                        app.github_scroll = app.github_scroll.saturating_add(1);
                                    } else {
                                        app.tmux_scroll = app.tmux_scroll.saturating_add(1);
                                    }
                                }
                                KeyCode::Enter => {
                                    app.github_expanded = !app.github_expanded;
                                    app.github_scroll = 0;
                                }
                                KeyCode::Left | KeyCode::Esc => {
                                    app.focus = FocusPane::Left;
                                }
                                KeyCode::Char('q') => break,
                                _ => {}
                            }
                        }
                    }
                }
                InputMode::Interact => {
                    if key.code == KeyCode::Esc {
                        app.input_mode = InputMode::Normal;
                        app.focus = FocusPane::Left;
                    } else if let Some(tmux_key) = key_to_tmux_send(key.code, key.modifiers) {
                        app.send_key_to_tmux(&tmux_key);
                    }
                }
                InputMode::CreateNew => match key.code {
                    KeyCode::Enter => app.confirm_create(),
                    KeyCode::Esc => {
                        app.input_mode = InputMode::Normal;
                        app.pending_new_branch.clear();
                        app.set_status("Cancelled");
                    }
                    _ => app.handle_input_key(key.code),
                },
                InputMode::SelectBaseBranch => match key.code {
                    KeyCode::Enter => app.confirm_base_branch(),
                    KeyCode::Esc => {
                        app.input_mode = InputMode::Normal;
                        app.set_status("Cancelled");
                    }
                    KeyCode::Down | KeyCode::Tab => {
                        let count = app.filtered_branches().len();
                        if count > 0 {
                            let i = app
                                .branch_list_state
                                .selected()
                                .map(|i| if i + 1 < count { i + 1 } else { 0 })
                                .unwrap_or(0);
                            app.branch_list_state.select(Some(i));
                        }
                    }
                    KeyCode::Up => {
                        let count = app.filtered_branches().len();
                        if count > 0 {
                            let i = app
                                .branch_list_state
                                .selected()
                                .map(|i| if i > 0 { i - 1 } else { count - 1 })
                                .unwrap_or(0);
                            app.branch_list_state.select(Some(i));
                        }
                    }
                    KeyCode::Backspace => {
                        app.branch_filter.pop();
                        app.branch_list_state
                            .select(if app.filtered_branches().is_empty() {
                                None
                            } else {
                                Some(0)
                            });
                    }
                    KeyCode::Char(c) => {
                        app.branch_filter.push(c);
                        app.branch_list_state
                            .select(if app.filtered_branches().is_empty() {
                                None
                            } else {
                                Some(0)
                            });
                    }
                    _ => {}
                },
                InputMode::Rename => match key.code {
                    KeyCode::Enter => app.confirm_rename(),
                    KeyCode::Esc => {
                        app.input_mode = InputMode::Normal;
                        app.set_status("Cancelled");
                    }
                    _ => app.handle_input_key(key.code),
                },
                InputMode::ConfirmDelete => match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => app.confirm_delete(),
                    _ => {
                        app.input_mode = InputMode::Normal;
                        app.set_status("Cancelled");
                    }
                },
                InputMode::ConfirmDeactivate => match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => app.confirm_deactivate(),
                    _ => {
                        app.input_mode = InputMode::Normal;
                        app.set_status("Cancelled");
                    }
                },
            }
        }

        // Periodic refresh (and always refresh preview in Interact mode)
        if app.input_mode == InputMode::Interact || last_refresh.elapsed() >= refresh_interval {
            if app.input_mode != InputMode::Interact
                && let Some(w) = app.worker.as_mut()
            {
                w.send(WorkerRequest::RefreshWorkstreams);
            }
            app.request_capture_pane();
            *last_refresh = Instant::now();
        }

        // Periodic git fetch (every 60s)
        if app.last_fetch.elapsed() >= Duration::from_secs(60) {
            let mut repo_paths: Vec<String> = app
                .workstreams
                .iter()
                .map(|w| w.repo_path.clone())
                .collect();
            repo_paths.sort();
            repo_paths.dedup();
            if !repo_paths.is_empty()
                && let Some(w) = app.worker.as_mut()
            {
                w.send(WorkerRequest::GitFetch { repo_paths });
            }
            app.last_fetch = Instant::now();
        }

        // Clear stale status messages (after 3 seconds)
        if let Some((_, created)) = &app.status_message
            && created.elapsed() > Duration::from_secs(3)
        {
            app.status_message = None;
        }
    }
    Ok(())
}

fn ui(f: &mut ratatui::Frame, app: &mut App) {
    let size = f.area();

    // Main layout: header + content area + status
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(3),    // content
            Constraint::Length(1), // status
        ])
        .split(size);

    draw_header(f, outer[0]);

    // Content: left 40% + right 60%
    let content = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(outer[1]);

    if app.input_mode == InputMode::SelectBaseBranch {
        draw_grouped_list(f, app, content[0]);
        draw_branch_picker(f, app, content[1]);
    } else {
        draw_grouped_list(f, app, content[0]);
        draw_right_panel(f, app, content[1]);
    }

    draw_status(f, app, outer[2]);

    // Help overlay on top of everything
    if app.show_help {
        draw_help_overlay(f, size);
    }
}

fn draw_header(f: &mut ratatui::Frame, area: Rect) {
    let version = env!("VEX_VERSION");
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " vex ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("v{version}"), Style::default().fg(Color::DarkGray)),
    ]));
    f.render_widget(header, area);
}

fn draw_grouped_list(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = if app.workstreams.is_empty() && app.loading.workstreams {
        vec![ListItem::new(Line::from(Span::styled(
            "(loading...)",
            Style::default().fg(Color::DarkGray),
        )))]
    } else if app.visible_rows.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "(no workstreams)",
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        app.visible_rows
            .iter()
            .map(|row| match row {
                ListRow::RepoHeader { repo_name } => ListItem::new(Line::from(Span::styled(
                    format!("{repo_name}/"),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                ))),
                ListRow::Workstream { index } => {
                    let ws = &app.workstreams[*index];
                    let pr = match ws.pr_number {
                        Some(n) => format!(" #{n}"),
                        None => String::new(),
                    };
                    let active_marker = if ws.active { " [active]" } else { "" };
                    let display = format!("  {}{}{}", ws.branch, pr, active_marker);
                    let style = if ws.active {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    ListItem::new(Line::from(Span::styled(display, style)))
                }
            })
            .collect()
    };

    let border_color = if app.focus == FocusPane::Left {
        Color::Cyan
    } else {
        Color::Blue
    };

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Workstreams ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color)),
        )
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::White),
        )
        .highlight_symbol("> ");

    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn draw_right_panel(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    if app.github_expanded {
        // Expanded: GitHub section takes 70%, tmux 30%
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
            .split(area);
        draw_github_section(f, app, chunks[0], true);
        draw_tmux_preview(f, app, chunks[1]);
    } else {
        // Compact: GitHub section 8 lines, rest is tmux
        let gh_height = 8u16.min(area.height.saturating_sub(4));
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(gh_height), Constraint::Min(3)])
            .split(area);
        draw_github_section(f, app, chunks[0], false);
        draw_tmux_preview(f, app, chunks[1]);
    }
}

fn draw_github_section(f: &mut ratatui::Frame, app: &App, area: Rect, expanded: bool) {
    let border_color = if app.focus == FocusPane::Right {
        Color::Cyan
    } else {
        Color::Blue
    };

    let Some(pr) = &app.github_pr_data else {
        // No PR data
        let has_pr = app.selected().and_then(|ws| ws.pr_number).is_some();
        let msg = if app.loading.pr_structured || has_pr {
            "(loading PR...)"
        } else {
            "(no PR)"
        };
        let block = Block::default()
            .title(" GitHub ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color));
        let para =
            Paragraph::new(Span::styled(msg, Style::default().fg(Color::DarkGray))).block(block);
        f.render_widget(para, area);
        return;
    };

    let mut lines: Vec<Line> = Vec::new();

    // Title line
    let state_color = match pr.state.as_str() {
        "OPEN" => Color::Green,
        "CLOSED" => Color::Red,
        "MERGED" => Color::Magenta,
        _ => Color::White,
    };
    lines.push(Line::from(vec![
        Span::styled(
            &pr.title,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" (#{})", pr.number),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(format!(" {}", pr.state), Style::default().fg(state_color)),
    ]));

    if expanded {
        // Full description
        if !pr.body.is_empty() {
            lines.push(Line::from(""));
            for desc_line in pr.body.lines().take(20) {
                lines.push(Line::from(Span::styled(
                    desc_line,
                    Style::default().fg(Color::Gray),
                )));
            }
        }

        // Reviews
        for (author, body, state) in &pr.reviews {
            if body.is_empty() && !matches!(state.as_str(), "CHANGES_REQUESTED" | "COMMENTED") {
                continue;
            }
            lines.push(Line::from(""));
            let state_indicator = match state.as_str() {
                "APPROVED" => " [APPROVED]",
                "CHANGES_REQUESTED" => " [CHANGES REQUESTED]",
                "COMMENTED" => " [COMMENT]",
                _ => "",
            };
            let state_color = match state.as_str() {
                "APPROVED" => Color::Green,
                "CHANGES_REQUESTED" => Color::Red,
                _ => Color::Yellow,
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" @{author}"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(state_indicator, Style::default().fg(state_color)),
            ]));
            if !body.is_empty() {
                for review_line in body.lines().take(10) {
                    lines.push(Line::from(Span::styled(
                        format!(" {review_line}"),
                        Style::default().fg(Color::Gray),
                    )));
                }
            }
            lines.push(Line::from(Span::styled(
                " ───────────────────────────────",
                Style::default().fg(Color::DarkGray),
            )));
        }

        // Comments
        for (author, body) in &pr.comments {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(" @{author}"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            for comment_line in body.lines().take(10) {
                lines.push(Line::from(Span::styled(
                    format!(" {comment_line}"),
                    Style::default().fg(Color::Gray),
                )));
            }
            lines.push(Line::from(Span::styled(
                " ───────────────────────────────",
                Style::default().fg(Color::DarkGray),
            )));
        }
    } else {
        // Compact: first 2 lines of body
        if !pr.body.is_empty() {
            for desc_line in pr.body.lines().take(2) {
                lines.push(Line::from(Span::styled(
                    desc_line,
                    Style::default().fg(Color::Gray),
                )));
            }
        }
    }

    // Checks summary
    lines.push(Line::from(""));
    let check_color = if pr.checks_passed == pr.checks_total && pr.checks_total > 0 {
        Color::Green
    } else if pr.checks_total == 0 {
        Color::DarkGray
    } else {
        Color::Yellow
    };
    lines.push(Line::from(Span::styled(
        format!("({}/{} checks passed)", pr.checks_passed, pr.checks_total),
        Style::default().fg(check_color),
    )));

    // URL
    if !pr.url.is_empty() {
        lines.push(Line::from(Span::styled(
            &pr.url,
            Style::default().fg(Color::Blue),
        )));
    }

    let title = format!(" PR #{} ", pr.number);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let para = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.github_scroll, 0));

    f.render_widget(para, area);
}

fn draw_tmux_preview(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let title = match app.selected() {
        Some(ws) => format!(" {} - {} ", app.config.default_window, ws.session),
        None => " Preview ".into(),
    };
    let border_color = if app.input_mode == InputMode::Interact {
        Color::Green
    } else if app.focus == FocusPane::Right {
        Color::Cyan
    } else {
        Color::Blue
    };

    let text = app
        .preview_content
        .as_bytes()
        .into_text()
        .unwrap_or_else(|_| Text::from(app.preview_content.as_str()));

    let preview = Paragraph::new(text)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color)),
        )
        .scroll((app.tmux_scroll, 0));

    f.render_widget(preview, area);
}

fn draw_help_overlay(f: &mut ratatui::Frame, area: Rect) {
    let help_lines = vec![
        Line::from(Span::styled(
            " Keybindings ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            " Left pane (workstream list):",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("  Up/Down     Navigate workstreams"),
        Line::from("  Enter       Open/attach workstream"),
        Line::from("  Right       Focus right pane"),
        Line::from("  n           New workstream"),
        Line::from("  r / s       Rename workstream"),
        Line::from("  Ctrl+D      Delete workstream"),
        Line::from("  Ctrl+X      Deactivate (kill session, keep worktree)"),
        Line::from("  ?           Show this help"),
        Line::from("  q / Esc     Quit"),
        Line::from(""),
        Line::from(Span::styled(
            " Right pane (preview):",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("  Up/Down     Scroll content"),
        Line::from("  Enter       Toggle GitHub expand/collapse"),
        Line::from("  Ctrl+O      Interact mode (keys to tmux)"),
        Line::from("  Left / Esc  Back to left pane"),
        Line::from(""),
        Line::from(Span::styled(
            " Interact mode:",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("  Esc         Exit interact mode"),
        Line::from("  (all keys)  Forwarded to tmux pane"),
        Line::from(""),
        Line::from(Span::styled(
            " Press any key to dismiss ",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let height = (help_lines.len() as u16 + 2).min(area.height.saturating_sub(4));
    let width = 50u16.min(area.width.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let overlay_area = Rect::new(x, y, width, height);

    f.render_widget(Clear, overlay_area);

    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let para = Paragraph::new(Text::from(help_lines))
        .block(block)
        .wrap(Wrap { trim: false });

    f.render_widget(para, overlay_area);
}

fn draw_branch_picker(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let filtered: Vec<ListItem> = app
        .filtered_branches()
        .iter()
        .map(|(_, b)| {
            ListItem::new(Line::from(Span::styled(
                b.to_string(),
                Style::default().fg(Color::White),
            )))
        })
        .collect();

    let title = format!(" Select base branch for '{}' ", app.pending_new_branch);
    let list = List::new(filtered)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Cyan),
        )
        .highlight_symbol("> ");

    f.render_stateful_widget(list, area, &mut app.branch_list_state);
}

fn draw_status(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let content = match &app.input_mode {
        InputMode::CreateNew => {
            let label = Span::styled("New branch: ", Style::default().fg(Color::Yellow));
            let input = Span::raw(&app.input_buffer);
            let cursor = Span::styled("_", Style::default().fg(Color::Yellow));
            Line::from(vec![label, input, cursor])
        }
        InputMode::SelectBaseBranch => {
            let label = Span::styled("Base branch: ", Style::default().fg(Color::Yellow));
            let input = Span::raw(&app.branch_filter);
            let cursor = Span::styled("_", Style::default().fg(Color::Yellow));
            Line::from(vec![label, input, cursor])
        }
        InputMode::Rename => {
            let label = Span::styled("Rename to: ", Style::default().fg(Color::Yellow));
            let input = Span::raw(&app.input_buffer);
            let cursor = Span::styled("_", Style::default().fg(Color::Yellow));
            Line::from(vec![label, input, cursor])
        }
        InputMode::ConfirmDelete => {
            let branch = app.selected().map(|w| w.branch.as_str()).unwrap_or("?");
            Line::from(Span::styled(
                format!("Delete '{branch}'? (y/N)"),
                Style::default().fg(Color::Red),
            ))
        }
        InputMode::ConfirmDeactivate => {
            let branch = app.selected().map(|w| w.branch.as_str()).unwrap_or("?");
            Line::from(Span::styled(
                format!("Deactivate '{branch}'? (y/N)"),
                Style::default().fg(Color::Yellow),
            ))
        }
        InputMode::Normal | InputMode::Interact => {
            if let Some((msg, _)) = &app.status_message {
                Line::from(Span::styled(
                    msg.as_str(),
                    Style::default().fg(Color::Yellow),
                ))
            } else {
                Line::from("")
            }
        }
    };

    let status = Paragraph::new(content);
    f.render_widget(status, area);
}
